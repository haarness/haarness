#!/usr/bin/env python3
"""Real TUI + daemon proof for deferred delivery and pending-message controls."""
import json, os, shlex, subprocess, tempfile, time, urllib.request
from pathlib import Path

def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)
def until(check, message):
    deadline=time.monotonic()+30
    while time.monotonic()<deadline:
        result=check()
        if result:return result
        time.sleep(.1)
    raise AssertionError(message)

binary=str(Path(os.environ.get('HAARNESS_BIN','target/debug/haarness')).resolve())
session=f'haarness-input-{os.getpid()}'
evidence=[]
with tempfile.TemporaryDirectory(prefix='haarness-input-tmux-') as directory:
    root=Path(directory);project=root/'project';config=project/'.haarness';config.mkdir(parents=True)
    (config/'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n')
    env=dict(os.environ,HAARNESS_DATA_DIR=str(root/'data'),HAARNESS_TEST_PROVIDER='1')
    command=shlex.join(['env',f"HAARNESS_DATA_DIR={env['HAARNESS_DATA_DIR']}",'HAARNESS_TEST_PROVIDER=1',binary,'--workspace',str(project),'--trust-project'])
    try:
        connection=json.loads(run(binary,'--workspace',str(project),'--trust-project','desktop',input='{"action":"connect"}',env=env))
        def api(path,body=None):
            req=urllib.request.Request(connection['url'].rstrip('/')+'/v1'+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+connection['token'],'Content-Type':'application/json'})
            with urllib.request.urlopen(req,timeout=5) as response:
                raw=response.read();return json.loads(raw) if raw else None
        def pane():return run('tmux','capture-pane','-p','-t',session)
        def keys(*values):run('tmux','send-keys','-t',session,*values)
        def text(value):run('tmux','send-keys','-l','-t',session,value)
        def send(value):text(value if value.startswith('/') else ' '+value);keys('Enter')
        def row(prefix):
            return next((i for i,line in enumerate(pane().splitlines()) if line.startswith(prefix)),None)
        def click(prefix):
            # SGR mouse press + release on the strip row, as a terminal reports it.
            index=until(lambda:row(prefix),f'{prefix!r} is not on screen')
            for final in 'Mm':text(f'\x1b[<0;5;{index+1}{final}')
        run('tmux','new-session','-d','-x','160','-y','48','-s',session,command)
        until(lambda:'h a a r n e s s' in pane(),'TUI did not open')
        send('!tool bash {"command":"sleep 60"}')
        turn=until(lambda:api('/turns/active'),'initial task did not start')[0]
        tid=turn['threadId']
        queue=lambda:api(f'/threads/{tid}/input/queue')
        # Steer is the default: a plain message joins the running work.
        send('steer while sleeping')
        until(lambda:'Steering · 1 — joins at the next step' in pane(),'steering strip missing')
        text('deferred one');keys('M-Enter')
        until(lambda:len(queue()['items'])==1,'Alt+Enter did not queue')
        text('deferred two');keys('M-Enter')
        until(lambda:len(queue()['items'])==2,'second prompt did not queue')
        until(lambda:'▎ Queued · 2' in pane(),'queue strip did not update')
        assert row('▎ 1. deferred one') is not None and row('▎ 2. deferred two') is not None, pane()
        assert not any(item['data'].get('text')=='deferred one' for item in api(f'/threads/{tid}/items'))
        evidence.append('WAITING\n'+pane())
        # Click a queued row to act on it; Edit loads it into the prompt and
        # sets the draft aside until the edit is saved.
        text(' my draft')
        click('▎ 1. deferred one')
        until(lambda:'Steer now' in pane(),'clicking a queued row did not open its actions')
        keys('Down','Enter')
        until(lambda:'Esc or click cancels and restores your draft' in pane(),'edit did not load into the prompt')
        assert 'Editing · Enter saves your changes' in pane(), pane()
        evidence.append('EDITING\n'+pane())
        text(' edited');keys('Enter')
        until(lambda:queue()['items'][0]['text']=='deferred one edited','edit did not persist')
        until(lambda:'my draft' in pane() and 'Editing' not in pane(),'draft was not restored after saving')
        assert [item['text'] for item in queue()['items']]==['deferred one edited','deferred two']
        keys('C-u')
        # /queue still lists everything; a listed action returns to the list.
        send('/queue');until(lambda:'Queued messages' in pane(),'queue menu missing')
        keys('Down','Enter');until(lambda:'Move up' in pane(),'second message has no Move up')
        keys('Down','Down','Down','Enter')
        until(lambda:queue()['items'][0]['text']=='deferred two','reorder did not persist')
        until(lambda:'Queued messages' in pane(),'reorder did not return to the list')
        keys('Enter');until(lambda:'discard it' in pane(),'remove action missing')
        keys('Down','Down','Enter')
        until(lambda:len(queue()['items'])==1,'remove did not persist')
        keys('Escape')
        until(lambda:'Queued messages' not in pane(),'queue list did not close')
        # Stop from the terminal (Esc twice): the queue pauses, and the unread
        # steering is answered instead of dropped.
        keys('Escape');until(lambda:'esc again to interrupt' in pane(),'first Esc did not arm Stop')
        keys('Escape')
        def steer_answered():
            items=api(f'/threads/{tid}/items')
            steer=next((i for i,item in enumerate(items) if (item['data'].get('text') or '').strip()=='steer while sleeping'),None)
            return steer is not None and any(item['data'].get('type')=='assistant_message' for item in items[steer+1:])
        # The answer is a continuation turn that starts once the stopped one
        # closes; give it time rather than racing the gap between them.
        try:answered=until(steer_answered,'')
        except AssertionError:answered=False
        until(lambda:not api('/turns/active'),'stop did not finish')
        until(lambda:"Queue on hold · send when you're ready" in pane(),'remaining queue was not parked')
        assert len(queue()['items'])==1
        evidence.append('STOPPED\n'+pane())
        # Resume the parked message through the terminal controls.
        send('/queue');until(lambda:'Queued messages' in pane(),'parked queue missing')
        keys('Enter');until(lambda:'Send now' in pane(),'parked message has no Send action')
        evidence.append('PARKED\n'+pane())
        keys('Enter')
        until(lambda:not queue()['items'],'Send did not drain parked input')
        until(lambda:not api('/turns/active'),'resumed input did not finish')
        until(lambda:'deferred one edited' in pane(),'resumed input missing in transcript')
        evidence.append('DELIVERED\n'+pane())
        Path('/tmp/haarness-input-queue-tmux.txt').write_text('\n\n'.join(evidence))
        assert answered, 'Stop (resume=true) did not answer the unread steering message'
        print('PASS: TUI steer, queue strip, click/edit in prompt, reorder, remove, Stop/park and Send; /tmp/haarness-input-queue-tmux.txt')
    except BaseException:
        try:Path('/tmp/haarness-input-queue-tmux-failure.txt').write_text(pane())
        except Exception:pass
        raise
    finally:
        subprocess.run(['tmux','kill-session','-t',session],capture_output=True)
        subprocess.run([binary,'--workspace',str(project),'serve','--stop'],env=env,capture_output=True,timeout=30)
