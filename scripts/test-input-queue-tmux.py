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
        run('tmux','new-session','-d','-x','160','-y','48','-s',session,command)
        until(lambda:'h a a r n e s s' in pane(),'TUI did not open')
        send('!tool bash {"command":"sleep 60"}')
        turn=until(lambda:api('/turns/active'),'initial task did not start')[0]
        permission={'threadId':turn['threadId'],'turnId':turn['id']}
        tid=turn['threadId']
        queue=lambda:api(f'/threads/{tid}/input/queue')
        text('deferred one');keys('M-Enter')
        until(lambda:len(queue()['items'])==1,'Alt+Enter did not queue')
        text('deferred two');keys('M-Enter')
        until(lambda:len(queue()['items'])==2,'second prompt did not queue')
        until(lambda:'2 queued' in pane(),'pending strip did not update')
        assert not any(item['data'].get('text')=='deferred one' for item in api(f'/threads/{tid}/items'))
        evidence.append('WAITING\n'+pane())
        send('/queue');until(lambda:'Queued messages' in pane(),'queue menu missing')
        keys('Enter');until(lambda:'Edit message' in pane(),'message actions missing')
        keys('Enter');until(lambda:'Edit queued message' in pane(),'queue editor missing')
        text(' edited');keys('Enter')
        until(lambda:queue()['items'][0]['text']=='deferred one edited','edit did not persist')
        keys('Down','Enter');until(lambda:'Move up' in pane(),'actions missing after edit')
        keys('Down','Down','Enter')
        until(lambda:queue()['items'][0]['text']=='deferred two','reorder did not persist')
        # First item -> Remove action.
        keys('Enter');until(lambda:'Remove message' in pane(),'remove action missing')
        keys('Down','Down','Down','Down','Enter')
        until(lambda:len(queue()['items'])==1,'remove did not persist')
        keys('Escape')
        api(f'/turns/{permission["turnId"]}/interrupt',{})
        until(lambda:not api('/turns/active'),'stop did not finish')
        until(lambda:'paused' in pane(),'remaining queue was not parked')
        assert len(queue()['items'])==1
        # Resume the selected parked message through the terminal controls.
        send('/queue');until(lambda:'Queued messages' in pane(),'parked queue missing')
        keys('Enter');until(lambda:'Send now' in pane(),'parked message has no Send action')
        evidence.append('PARKED\n'+pane())
        keys('Down','Enter')
        until(lambda:not queue()['items'],'Send did not drain parked input')
        until(lambda:not api('/turns/active'),'resumed input did not finish')
        until(lambda:'deferred one edited' in pane(),'resumed input missing in transcript')
        evidence.append('DELIVERED\n'+pane())
        Path('/tmp/haarness-input-queue-tmux.txt').write_text('\n\n'.join(evidence))
        print('PASS: TUI queue, edit, reorder, remove, Stop/park and Send; /tmp/haarness-input-queue-tmux.txt')
    except BaseException:
        try:Path('/tmp/haarness-input-queue-tmux-failure.txt').write_text(pane())
        except Exception:pass
        raise
    finally:
        subprocess.run(['tmux','kill-session','-t',session],capture_output=True)
        subprocess.run([binary,'--workspace',str(project),'serve','--stop'],env=env,capture_output=True,timeout=30)
