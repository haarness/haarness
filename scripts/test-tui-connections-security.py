#!/usr/bin/env python3
"""Real daemon + tmux coverage for interactive configuration and composer parity.
All changes are confined to a fresh data root; provider traffic uses local fixtures.
"""
import http.server,json,os,pathlib,shlex,subprocess,tempfile,threading,time,urllib.request,urllib.error,uuid
binary=pathlib.Path(os.environ.get('HAARNESS_BIN','target/debug/haarness')).resolve()
root=pathlib.Path(tempfile.mkdtemp(prefix='haarness-tui-parity-')).resolve()
workspace=root/'workspace'; workspace.mkdir()
config=root/'config';config.mkdir()
(config/'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\nexternal_directory="allow"\n[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n[tui.keybinds]\nusage="f6"\n')
(workspace/'sample.txt').write_text('line one\nline two\nline three\nline four\n')
(workspace/'.haarness/agents').mkdir(parents=True)
(workspace/'.haarness/agents/worker.md').write_text('---\ndescription: Fixture worker\nmode: subagent\n---\nDo the requested work.')
class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        data=json.dumps({'data':[{'id':'fixture-model'}]}).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(data)
service=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=service.serve_forever,daemon=True).start()
env={**os.environ,'HAARNESS_DATA_DIR':str(root),'HAARNESS_TEST_PROVIDER':'1','HAARNESS_SCHEDULE_SKIP_INSTALL':'1'}
for key in ['OPENAI_API_KEY','ANTHROPIC_API_KEY','GOOGLE_API_KEY','GEMINI_API_KEY','XAI_API_KEY']:env.pop(key,None)
name='haarness-parity-'+str(os.getpid());endpoint=None

def run(args,**kwargs):return subprocess.run(args,check=True,text=True,capture_output=True,**kwargs).stdout

def api(path,body=None,token=True):
    request=urllib.request.Request(endpoint['url']+'/v1'+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json',**({'Authorization':'Bearer '+endpoint['token']} if token else {})})
    with urllib.request.urlopen(request,timeout=25) as response:return json.load(response)
def manage(resource,**input):return api('/manage/'+resource,input)
def form(resource,changes,**input):
    view=manage(resource,action='form',**input);request={**view['submit'],**{f['key']:f['value'] for f in view['fields']},**changes};return api('/manage/'+resource,request)
def until(predicate,label,seconds=35):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        try:
            result=predicate()
            if result:return result
        except (urllib.error.HTTPError,urllib.error.URLError):pass
        time.sleep(.15)
    raise AssertionError(label+'\n'+capture())
def capture():
    result=subprocess.run(['tmux','capture-pane','-p','-t',name+':0.0'],text=True,capture_output=True)
    return result.stdout

def keys(*args):
    for key in args:
        run(['tmux','send-keys','-t',name+':0.0',key])
        if key=='Escape':time.sleep(.15)
def literal(value):run(['tmux','send-keys','-l','-t',name+':0.0',value])
def command(value):keys('Escape','C-u');literal(value);keys('Enter')
def choose(value):literal(value);keys('Enter');time.sleep(.15)
def fill(label,value):choose(label);keys('C-u');literal(value);keys('Enter')
def evidence(label): (root/(label+'.txt')).write_text(capture())

endpoint2=None;env2=None;outside=pathlib.Path.home()/('haarness-external-write-test-'+str(uuid.uuid4()))
def tool(command):
    thread=api('/threads',{'provider':'echo','model':'echo-1','agent':'build'})
    api('/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':'!tool bash '+json.dumps({'command':command}),'attachments':[]})
    result=until(lambda:next((i['data'] for i in api('/threads/'+thread['id']+'/items') if i['data']['type']=='tool_result'),None),'Tool result missing')
    return result

def public(token):
    try:
        with urllib.request.urlopen(endpoint['url']+'/s/'+token,timeout=10) as r:return r.status,r.read().decode(),{k.lower():v for k,v in r.headers.items()}
    except urllib.error.HTTPError as e:return e.code,e.read().decode(),dict(e.headers)
def palette(value):keys('Escape','C-p');choose(value)
try:
    endpoint=json.loads(run([str(binary),'--workspace',str(workspace),'--trust-project','desktop'],env=env,input='{"action":"connect"}',timeout=120))
    # Ordinary execution uses host permissions. Retired policy files cannot block it.
    legacy=root/'data/sandbox.json'
    legacy.write_text('{"profile":"offline","allowedHosts":[]}')
    value=tool('printf allowed > ordinary-inside.txt');assert not value['is_error'],value
    assert (workspace/'ordinary-inside.txt').read_text()=='allowed'
    value=tool('printf allowed > '+shlex.quote(str(outside)));assert outside.read_text()=='allowed',value
    value=tool('curl -fsS --max-time 5 --noproxy "*" http://127.0.0.1:'+str(service.server_port));assert not value['is_error'] and 'fixture-model' in value['output'],value
    legacy.write_text('invalid retired configuration')
    value=tool('printf configuration-is-ignored');assert 'configuration-is-ignored' in value['output'],value
    try:manage('sandbox',action='view');raise AssertionError('Removed settings are still available')
    except urllib.error.HTTPError:pass
    thread=api('/threads',{'provider':'echo','model':'echo-1','agent':'build','title':'Frozen shared conversation'})
    api('/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':'shared <script>hello</script> marker','attachments':[]})
    until(lambda:any(i['data']['type']=='assistant_message' for i in api('/threads/'+thread['id']+'/items')),'Share fixture incomplete')
    preview=manage('sharing',action='preview',threadId=thread['id'],includeTools=False);request=preview['entries'][0]['request'];token=request['preview'];assert public(token)[0]==404
    api('/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':'not-in-frozen-preview','attachments':[]})
    until(lambda:any(i['data'].get('text')=='not-in-frozen-preview' for i in api('/threads/'+thread['id']+'/items')),'Second message missing')
    created=api('/manage/sharing',request);status,body,headers=public(token)
    assert status==200 and '&lt;script&gt;' in body and '<script>' not in body and 'not-in-frozen-preview' not in body
    assert headers['cache-control']=='no-store' and "default-src 'none'" in headers['content-security-policy']
    manage('sharing',action='revoke',threadId=thread['id'],token=token);assert public(token)[0]==404
    # Time-bounded public links and private previews are rejected after expiration.
    import datetime
    old=(datetime.datetime.now(datetime.timezone.utc)-datetime.timedelta(days=1)).isoformat()
    again=manage('sharing',action='preview',threadId=thread['id'],includeTools=False)['entries'][0]['request'];expired=again['preview']
    private=root/'data/share-previews/shares'/(expired+'.json');snapshot=json.loads(private.read_text());snapshot['createdAt']=old;private.write_text(json.dumps(snapshot))
    try:api('/manage/sharing',again);raise AssertionError('Expired preview published')
    except urllib.error.HTTPError:pass
    again=manage('sharing',action='preview',threadId=thread['id'],includeTools=False)['entries'][0]['request'];expired=again['preview'];api('/manage/sharing',again)
    publicfile=root/'data/shares'/(expired+'.json');snapshot=json.loads(publicfile.read_text());snapshot['expiresAt']=old;publicfile.write_text(json.dumps(snapshot));assert public(expired)[0]==404
    # Another actual host, then a second project behind its authenticated gateway.
    second=root/'server-two';(second/'workspace').mkdir(parents=True);(second/'config').mkdir();(second/'config/config.toml').write_text((config/'config.toml').read_text())
    env2={**env,'HAARNESS_DATA_DIR':str(second),'HAARNESS_SERVER_PASSWORD':'second-host-password','HAARNESS_SERVER_TOKEN':'second-host-private-token'}
    run([str(binary),'--workspace',str(second/'workspace'),'--trust-project','serve','--daemon','--port','0'],env=env2,timeout=120)
    endpoint2=json.loads(run([str(binary),'--workspace',str(second/'workspace'),'--trust-project','desktop'],env=env2,input='{"action":"connect"}',timeout=120))
    original=endpoint;endpoint=endpoint2
    other=api('/threads',{'provider':'echo','model':'echo-1','agent':'build','title':'SERVER TWO ONLY'})
    endpoint=original
    args=['env','HAARNESS_DATA_DIR='+str(root),'HAARNESS_TEST_PROVIDER=1',str(binary),'--workspace',str(workspace),'--trust-project']
    run(['tmux','new-session','-d','-x','155','-y','44','-s',name,shlex.join(args)]);until(lambda:'Echo 1' in capture(),'TUI failed to start')
    command('/servers');choose('Add server');fill('Name','Second test host');fill('Server URL',endpoint2['url']);fill('Password','second-host-password');choose('Save');until(lambda:'Second test host' in capture(),'Server not saved');keys('Escape')
    literal('draft belongs to original');palette('servers');choose('Second test host');choose('Connect');until(lambda:'Connected to selected server' in capture(),'Server switch failed');assert 'draft belongs to original' not in capture()
    palette('sessions');until(lambda:'SERVER TWO ONLY' in capture(),'Target catalog missing');assert 'Frozen shared conversation' not in capture();keys('Escape')
    literal('draft belongs to second');palette('servers');choose('Original server');until(lambda:'draft belongs to original' in capture(),'Original draft not restored');evidence('original-server-restored')
    palette('servers');choose('Second test host');choose('Connect');until(lambda:'draft belongs to second' in capture(),'Second draft not restored');evidence('second-server-restored')
    project=second/'another-project';project.mkdir();palette('servers');choose('Projects on this server');choose('Open another folder');fill('Absolute folder path',str(project));choose('Save');until(lambda:'Connected to selected server' in capture() and 'draft belongs to second' not in capture(),'Remote project did not open');evidence('remote-project')
    literal('remote project message marker');keys('Enter');until(lambda:'Echo: remote project message marker' in capture(),'Remote project turn failed')
    palette('servers');choose('Projects on this server');until(lambda:'another-project' in capture(),'Project catalog missing');evidence('server-projects')
    assert 'draft belongs to original' not in capture()
    keys('Escape');literal('draft survives remote client restart');until(lambda:'draft survives remote client restart' in capture(),'Restart draft was not entered');evidence('remote-draft-before-restart');palette('quit')
    until(lambda:subprocess.run(['tmux','has-session','-t',name],capture_output=True).returncode!=0,'TUI did not exit')
    run(['tmux','new-session','-d','-x','155','-y','44','-s',name,shlex.join(args)]);until(lambda:'Echo 1' in capture(),'TUI restart failed')
    palette('servers');choose('Second test host');choose('Connect');until(lambda:'draft belongs to second' in capture(),'Saved host draft missing after restart')
    palette('servers');choose('Projects on this server');choose('another-project');until(lambda:'draft survives remote client restart' in capture(),'Project draft missing after restart');evidence('remote-draft-after-restart')
    (root/'result.json').write_text(json.dumps({'passed':True,'checks':['ordinary workspace and permitted external writes','host networking','retired policy ignored','private frozen share preview','public HTML escaping','share revoke','tmux server switching','per-server drafts','host project gateway and real turn']},indent=2))
    print('PASS connections, permissions, sharing and tmux. Evidence: '+str(root),flush=True)
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    if endpoint:subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=35)
    if endpoint2:subprocess.run([str(binary),'--workspace',str(second/'workspace'),'serve','--stop'],env=env2,capture_output=True,timeout=35)
    if env2 and (second/'another-project').exists():subprocess.run([str(binary),'--workspace',str(second/'another-project'),'serve','--stop'],env=env2,capture_output=True,timeout=35)
    outside.unlink(missing_ok=True);service.shutdown();service.server_close();print('Artifacts: '+str(root),flush=True)
