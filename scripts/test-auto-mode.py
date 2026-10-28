#!/usr/bin/env python3
"""Real Haarness server + real OpenTUI in tmux: Auto/Review/Yolo permission
lifecycle, the approval card at several terminal sizes, the --auto, --approvals and
--yolo flags, the mode a chat with no setting of its own starts in (Review when
the person's configuration says so, whatever the project says; Auto when no file
sets it), and the mode /new and /archive hand to the chat they open.

HAARNESS_E2E_FRAMES names a folder for captured frames (ANSI and text);
by default they land beside the other evidence."""
import json, os, pathlib, re, shlex, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch, e2e_tmux

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
    # The pane runs with this script's environment, not the tmux server's.
    args=[str(binary),'--workspace',str(folder),'--trust-project',*extra,'--thread',thread]
    run(['tmux','new-session','-d','-x',str(width),'-y',str(height),'-s',name,e2e_tmux.pane_command(environment,args)])
    until(lambda:mode in capture(), mode+' mode not visible at launch')

def danger(label):
    """Whether the band draws `label` in a red (the theme's danger colour):
    red well above green and blue, which sit close together, unlike the
    copper accent."""
    for row in run(['tmux','capture-pane','-p','-e','-t',name+':0.0']).split('\n'):
        if label in re.sub(r'\x1b\[[0-9;]*m','',row):
            styled=re.search(r'38;2;(\d+);(\d+);(\d+)m(?:\x1b\[[0-9;]*m)*\s*'+re.escape(label),row)
            if not styled:
                return False
            red,green,blue=map(int,styled.groups())
            return red-max(green,blue)>=60 and abs(green-blue)<=30
    return False

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

def opened_by(text):
    """Run /new or /archive in the TUI and answer the chat it opened."""
    before={item['id'] for item in api('/v1/threads')}
    command(text)
    until(lambda:{item['id'] for item in api('/v1/threads')}-before,text+' opened no chat')
    (opened,)={item['id'] for item in api('/v1/threads')}-before
    return opened

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
    # Ctrl+Y goes on to Yolo, then back to Review.
    keys('C-y')
    until(lambda:api('/v1/threads/'+thread+'/approval-mode')['mode']=='yolo','Ctrl+Y did not go from Auto to Yolo')
    keys('C-y')
    until(lambda:'asks before risky steps' in capture(),'Ctrl+Y did not return to Review')
    assert api('/v1/threads/'+thread+'/approval-mode')['mode']=='review'
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
    check('Allow once, then Auto asks to confirm, then executes the pending request and enables Auto')
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
    command('/approvals review')
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

    # Yolo: Ctrl+Y goes Review → Auto → Yolo → Review. Yolo approves what
    # waits, even what the reviewer left to a person, and every new request
    # once, without a card or a review; deny rules still refuse.
    run(['tmux','kill-session','-t',name])
    yolo=api('/v1/threads', {'title':'Yolo verification','provider':'echo','model':'echo-1'})['id']
    assert api('/v1/threads/'+yolo+'/approval-mode')['mode']=='review'
    assert 'approval-mode.yolo.v1' in api('/v1/info')['capabilities']
    start(yolo,'!tool bash '+json.dumps({'command':'printf waited-proof > waited.txt'})+' [review:ask]')
    until(lambda:len(api('/v1/permissions'))==1,'Review did not ask')
    launch(yolo)
    keys('C-y')
    until(lambda:'ctrl+y for Yolo' in capture(),'Ctrl+Y did not go to Auto, naming Yolo next')
    until(lambda:(api('/v1/permissions') or [{}])[0].get('review',{}).get('status')=='asked','the reviewer did not leave the request to a person')
    assert not (workspace/'waited.txt').exists()
    keys('C-y')
    until(lambda:'approvals: yolo' in capture() or 'Yolo on for this chat' in capture(),'Ctrl+Y did not go to Yolo')
    proof('waited.txt','waited-proof')
    assert api('/v1/threads/'+yolo+'/approval-mode')['mode']=='yolo'
    check('Ctrl+Y goes Review → Auto → Yolo, and Yolo approves the request Auto\'s reviewer left to a person')
    start(yolo,'!tool bash '+json.dumps({'command':'printf yolo-proof > yolo.txt'})+' [review:deny]')
    proof('yolo.txt','yolo-proof')
    assert not api('/v1/permissions')
    items=json.dumps(api('/v1/threads/'+yolo+'/items'))
    assert 'Auto review blocked' not in items, items
    start(yolo,'!tool bash '+json.dumps({'command':'printf denied-proof > denied.txt'}))
    finished()
    assert not (workspace/'denied.txt').exists() and not api('/v1/permissions')
    check('in Yolo a request runs without a card or a review, and a deny rule still refuses')
    until(lambda:'approvals: yolo' in capture(),'the band does not name Yolo')
    assert danger('approvals: yolo'), run(['tmux','capture-pane','-p','-e','-t',name+':0.0'])
    save('yolo-band')
    check('the band names Yolo in the danger colour')
    keys('C-y')
    until(lambda:'asks before risky steps' in capture(),'Ctrl+Y did not go back to Review')
    assert api('/v1/threads/'+yolo+'/approval-mode')['mode']=='review'
    start(yolo,'!tool bash '+json.dumps({'command':'printf yolo-proof > yolo.txt'}))
    until(lambda:len(api('/v1/permissions'))==1,'Review did not ask after Yolo')
    keys('n')
    finished()
    check('Ctrl+Y returns from Yolo to Review, which asks again: Yolo approved once and remembered nothing')
    command('/approvals yolo')
    until(lambda:api('/v1/threads/'+yolo+'/approval-mode')['mode']=='yolo','/approvals yolo did not set Yolo')
    until(lambda:'approvals: yolo' in capture() or 'Yolo on for this chat' in capture(),'the TUI does not show Yolo')
    api('/v1/threads/'+yolo+'/approval-mode',{'mode':'review'},'PUT')
    until(lambda:'approvals: review' in capture() or 'review' in capture().split('\n')[-2],'the TUI did not follow Review set elsewhere')
    check('/approvals yolo sets Yolo, and the TUI follows a change made elsewhere')
    run(['tmux','kill-session','-t',name])
    start(yolo,'!tool bash '+json.dumps({'command':'printf flag-yolo-proof > flag-yolo.txt'}))
    until(lambda:len(api('/v1/permissions'))==1,'Review did not ask before --approvals yolo')
    launch(yolo,'approvals: yolo',extra=['--approvals','yolo'])
    proof('flag-yolo.txt','flag-yolo-proof')
    assert api('/v1/threads/'+yolo+'/approval-mode')['mode']=='yolo'
    check('haarness --approvals yolo opens the chat in Yolo on the server and approves what waits')
    run([str(binary),'--workspace',str(workspace),'run','--approvals','yolo','--title','Run yolo flag','!tool bash '+json.dumps({'command':'printf run-yolo-proof > run-yolo.txt'})+' [review:deny]'],env=env,input='',timeout=120)
    proof('run-yolo.txt','run-yolo-proof')
    ran=[item['id'] for item in api('/v1/threads') if item['title']=='Run yolo flag']
    assert len(ran)==1 and api('/v1/threads/'+ran[0]+'/approval-mode')['mode']=='yolo', ran
    check('haarness run --approvals yolo keeps its chat in Yolo, so a run nobody watches gets every request approved')
    run(['tmux','kill-session','-t',name])
    plain=api('/v1/threads', {'title':'Yolo flag verification','provider':'echo','model':'echo-1'})['id']
    launch(plain,'approvals: --yolo',extra=['--yolo'])
    start(plain,'!tool bash '+json.dumps({'command':'printf client-yolo-proof > client-yolo.txt'}))
    proof('client-yolo.txt','client-yolo-proof')
    assert api('/v1/threads/'+plain+'/approval-mode')['mode']=='review'
    check('--yolo still answers for this TUI only and leaves the chat in Review')
    # That Review comes from the person's configuration, not from a choice, so
    # the chat /new opens is left to follow the configuration too.
    opened=opened_by('/new')
    time.sleep(1)
    mode=api('/v1/threads/'+opened+'/approval-mode')
    assert mode['mode']=='review' and not mode['explicit'], mode
    check('/new from a chat in the configured Review leaves the new chat to the configuration')

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
    launch(flagged,'  auto',extra=['--auto'])
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
    launch(fresh,'  auto',folder=unset,environment=unset_env)
    check('the TUI opens a chat with no setting in Auto')
    plain=subprocess.run([str(binary),'--workspace',str(unset),'run','--title','Unset run','!tool bash '+json.dumps({'command':'printf unset-run-proof > unset-run.txt'})],env=unset_env,input='',text=True,capture_output=True,timeout=120)
    assert plain.returncode==0, plain.stderr
    proof('unset-run.txt','unset-run-proof',unset)
    check('a plain haarness run there gets its ask answered by the reviewer instead of refused')

    # Here the default is Auto, so a Review the person picks is the mode a
    # chat opened in its place could lose: /new and /archive hand it on, as
    # they hand on Yolo.
    assert api('/v1/info')['defaultApprovalMode']=='auto'
    command('/approvals review')
    until(lambda:api('/v1/threads/'+fresh+'/approval-mode')['mode']=='review','/approvals review did not set Review')
    opened=opened_by('/new')
    until(lambda:api('/v1/threads/'+opened+'/approval-mode')['mode']=='review','/new did not keep the Review chosen for the chat it replaced')
    assert api('/v1/threads/'+opened+'/approval-mode')['explicit']
    until(lambda:'approvals: review' in capture().split('\n')[-2],'the TUI does not show the new chat in Review')
    check('/new keeps a Review the person chose where the default is Auto')
    replaced=opened
    opened=opened_by('/archive')
    until(lambda:api('/v1/threads/'+opened+'/approval-mode')['mode']=='review','/archive did not keep the Review of the chat it archived')
    assert replaced not in {item['id'] for item in api('/v1/threads')}
    command('/approvals yolo')
    until(lambda:api('/v1/threads/'+opened+'/approval-mode')['mode']=='yolo','/approvals yolo did not set Yolo')
    opened=opened_by('/archive')
    until(lambda:api('/v1/threads/'+opened+'/approval-mode')['mode']=='yolo','/archive did not keep the Yolo of the chat it archived')
    until(lambda:'approvals: yolo' in capture(),'the TUI does not show the new chat in Yolo')
    save('archive-keeps-yolo')
    check('/archive opens the next chat in the archived chat\'s Review or Yolo')
    print(f'{len(checks)} real-server TUI scenarios passed.',flush=True)
    (root/'tui.txt').write_text(capture())
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=30)
    subprocess.run([str(binary),'--workspace',str(unset),'serve','--stop'],env=unset_env,capture_output=True,timeout=30)
    e2e_scratch.settle(root)
