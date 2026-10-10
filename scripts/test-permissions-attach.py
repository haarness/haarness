#!/usr/bin/env python3
"""Real server and attached TUI: ordinary tools, manual approvals and retired policy migration."""
import http.server, json, os, pathlib, shlex, subprocess, tempfile, threading, time, urllib.request, urllib.error

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-permissions-attach-')).resolve()
workspace = root/'server-project'; workspace.mkdir()
local = root/'client-project'; local.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
(state/'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash = { "printf blocked*" = "deny", "printf review*" = "ask" }\n')
legacy = state/'data/sandbox.json'; legacy.write_text('{"profile":"offline","allowedHosts":[]}')
(workspace/'.env').write_text('TEST_ONLY=read-approval-proof')
(workspace/'service.env').write_text('TEST_ONLY=named-env-proof')
(workspace/'.env.example').write_text('EXAMPLE=public-proof')
env = {k:v for k,v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_SERVER_TOKEN='', HAARNESS_SERVER_PASSWORD='', HAARNESS_TEST_PROVIDER='1', HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1')
name = 'haarness-permissions-'+str(os.getpid()); url = None; checks = []
class Web(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b'host-network-proof')
web = http.server.ThreadingHTTPServer(('127.0.0.1',0),Web)
threading.Thread(target=web.serve_forever,daemon=True).start()
def run(args,**kwargs): return subprocess.run(args,check=True,text=True,capture_output=True,timeout=40,**kwargs).stdout
def api(path,body=None):
    req = urllib.request.Request(url+'/v1'+path, data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req,timeout=10) as reply:
        data=reply.read(); return json.loads(data) if data else None
def capture(): return subprocess.run(['tmux','capture-pane','-p','-t',name+':0.0'],text=True,capture_output=True).stdout
def until(predicate,label):
    deadline=time.monotonic()+25
    while time.monotonic()<deadline:
        if result:=predicate(): return result
        time.sleep(.1)
    raise AssertionError(label+'\n'+capture())
def keys(*args): run(['tmux','send-keys','-t',name,*args])
def send(text):
    run(['tmux','send-keys','-l','-t',name,text]); keys('Enter')
def items(): return api('/threads/'+thread+'/items')
def start(tool,args):
    before = sum(item['data']['type']=='tool_result' for item in items())
    # Leading whitespace keeps the deterministic tool directive in the chat composer.
    send(' !tool '+tool+' '+json.dumps(args))
    return before

def finish(before):
    result=until(lambda: (values[-1] if len(values:=[item['data'] for item in items() if item['data']['type']=='tool_result'])>before else None),'tool result missing')
    until(lambda:not api('/turns/active'),'turn still active')
    return result

def prompt(category):
    until(lambda:any(p['permission']==category for p in api('/permissions')),'permission missing: '+category)
    until(lambda:'Allow once' in capture() or 'allow once' in capture(),'approval card missing')

def check(label):
    checks.append(label); (root/(str(len(checks))+'.txt')).write_text(capture()); print('PASS '+label,flush=True)
try:
    run([str(binary),'--workspace',str(workspace),'serve','--daemon','--port','0'],env=env)
    registry=json.loads((state/'data/workspaces.json').read_text()); identity=next(item['id'] for item in registry['workspaces'] if item['path']==str(workspace))
    url=json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())['url']
    thread=api('/threads',{'title':'Attached permission verification','provider':'echo','model':'echo-1','agent':'build'})['id']
    args=['env', *[key+'='+value for key,value in env.items() if key.startswith('HAARNESS_')], str(binary),'--workspace',str(local),'--session',thread,'tui','--attach',url]
    run(['tmux','new-session','-d','-x','160','-y','44','-s',name,shlex.join(args)])
    until(lambda:'Echo 1' in capture(),'attached TUI missing')
    before=start('bash',{'command':'printf ordinary-proof > ordinary.txt'}); assert not finish(before)['is_error']
    assert (workspace/'ordinary.txt').read_text()=='ordinary-proof' and not (local/'ordinary.txt').exists()
    assert not api('/permissions'); check('default shell runs on server without approval or OS restrictions')
    before=start('bash',{'command':f'curl -fsS --max-time 5 --noproxy "*" http://127.0.0.1:{web.server_port}'})
    assert 'host-network-proof' in finish(before)['output']; check('old Offline policy does not restrict shell networking')
    legacy.write_text('{"profile":"strict","allowedHosts":[]}')
    before=start('webfetch',{'url':f'http://127.0.0.1:{web.server_port}'})
    assert 'host-network-proof' in finish(before)['output']; check('old Strict policy does not restrict web fetching')
    legacy.write_text('malformed retired configuration')
    outside=root/'outside.txt'
    before=start('write',{'path':str(outside),'content':'approved-external-proof'}); prompt('external_directory')
    assert not outside.exists(); keys('y'); assert not finish(before)['is_error']; assert outside.read_text()=='approved-external-proof'
    check('external writes wait for Allow once then execute, even with stale malformed policy')
    before=start('write',{'path':str(outside),'content':'denied-external-proof'}); prompt('external_directory'); keys('n'); assert finish(before)['is_error']; assert outside.read_text()=='approved-external-proof'
    check('Allow once is not remembered and Deny prevents the write')
    for filename in ['.env','service.env']:
        before=start('read',{'path':filename}); prompt('read'); keys('y'); assert not finish(before)['is_error']
    before=start('read',{'path':'.env.example'}); assert 'EXAMPLE=public-proof' in finish(before)['output']; assert not api('/permissions')
    check('environment secrets require approval while examples run normally')
    before=start('bash',{'command':'printf blocked > blocked.txt'}); assert finish(before)['is_error']; assert not (workspace/'blocked.txt').exists(); assert not api('/permissions')
    check('configured Deny prevents execution without a bypass prompt')
    before=start('bash',{'command':'printf review-one > first.txt'}); prompt('bash'); keys('a'); until(lambda:'until the Haarness server stops' in capture(),'Always confirmation missing'); keys('Enter'); assert not finish(before)['is_error']
    before=start('bash',{'command':'printf review-two > second.txt'}); assert not finish(before)['is_error']; assert (workspace/'second.txt').read_text()=='review-two'; assert not api('/permissions')
    check('Always approval retains command-prefix permission for the session')
    send('/sandbox'); until(lambda:'Unknown command /sandbox' in capture(),'retired command was not reported as unknown'); assert 'Tool sandbox' not in capture()
    assert not any(item['data'].get('text','').strip()=='/sandbox' for item in items()),'unknown command reached the model'; check('sandbox command is removed')
    try: api('/manage/sandbox',{'action':'save','profile':'offline'}); raise AssertionError('retired API accepted')
    except urllib.error.HTTPError as error: assert error.code in (400,404,422),error.code
    (root/'items.json').write_text(json.dumps(items(),indent=2)); (root/'result.json').write_text(json.dumps({'passed':True,'checks':checks},indent=2))
    print('PASS '+str(len(checks))+' attached TUI scenarios. Evidence: '+str(root),flush=True)
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    if url: subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=35)
    web.shutdown(); web.server_close(); print('Artifacts: '+str(root),flush=True)
