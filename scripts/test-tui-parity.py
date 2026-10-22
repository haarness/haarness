#!/usr/bin/env python3
"""Real daemon + tmux coverage for interactive configuration and composer parity.
All changes are confined to a fresh data root; provider traffic uses local fixtures.
"""
import http.server,json,os,pathlib,shlex,subprocess,tempfile,threading,time,urllib.request,urllib.error,uuid
import e2e_scratch
import e2e_tmux
binary=pathlib.Path(os.environ.get('HAARNESS_BIN','target/debug/haarness')).resolve()
root=pathlib.Path(tempfile.mkdtemp(prefix='haarness-tui-parity-')).resolve()
workspace=root/'workspace'; workspace.mkdir()
config=root/'config';config.mkdir()
(config/'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n[tui.keybinds]\nusage="f6"\n')
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

try:
    endpoint=json.loads(run([str(binary),'--workspace',str(workspace),'--trust-project','desktop'],env=env,input='{"action":"connect"}',timeout=120))
    try:api('/manage/memory',{'action':'view'},token=False);raise AssertionError('Unauthenticated management accepted')
    except urllib.error.HTTPError as error:assert error.code==401
    # Shared forms persist through the existing domain services.
    form('memory',{'name':'fixture','description':'Original description','content':'original content','type':'project'})
    details=manage('memory',action='inspect',name='fixture');assert 'original content' in '\n'.join(details['lines'])
    edit=manage('memory',action='form',name='fixture');stale={**edit['submit'],**{f['key']:f['value'] for f in edit['fields']},'content':'stale write'}
    form('memory',{'content':'updated content'},name='fixture')
    try:api('/manage/memory',stale);raise AssertionError('Stale memory write accepted')
    except urllib.error.HTTPError:pass
    assert 'updated content' in '\n'.join(manage('memory',action='inspect',name='fixture')['lines'])
    form('hooks',{'scope':'global','event':'PostToolUse','matcher':'bash','command':'printf hook >> hook-hit.txt','timeout':'10'})
    hooks=manage('hooks',action='view');hook=next(e['request'] for e in hooks['entries'] if e['request']['action']=='inspect')
    detail=api('/manage/hooks',hook);assert any(e['label']=='Edit' for e in detail['entries'])
    # Capture and restore from the server's immutable profile catalog.
    form('profiles',{'name':'fixture-profile','scope':'global'})
    profiles=manage('profiles',action='view');profile=next(e['request'] for e in profiles['entries'] if e['request']['action']=='inspect')
    assert manage('profiles',action='inspect',id=profile['id'])['title']=='fixture-profile'
    # Custom provider and masked credential management: no external provider traffic.
    form('providers',{'name':'fixture-ui','label':'Local provider fixture','url':f'http://127.0.0.1:{service.server_port}/v1','protocol':'openai-chat-completions','models':'fixture-model','requiresAuth':'false'})
    until(lambda:any(p['id']=='fixture-ui' for p in api('/providers')),'Custom provider did not hot reload')
    form('providers',{'key':'fixture-secret-not-for-display'},name='fixture-ui')
    assert 'fixture-secret-not-for-display' not in json.dumps(manage('providers',action='inspect',name='fixture-ui'))
    assert (root/'data/auth.json').stat().st_mode & 0o077 == 0
    manage('providers',action='logout',name='fixture-ui')
    # A real stdio MCP process, reloaded and reconnected by the server.
    mcp_file=root/'mcp-fixture.py'
    mcp_file.write_text("""import sys,json
for line in sys.stdin:
 try:
  request=json.loads(line)
  if 'id' not in request:continue
  method=request.get('method')
  result={'protocolVersion':request.get('params',{}).get('protocolVersion','2024-11-05'),'capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}} if method=='initialize' else {'tools':[]} if method=='tools/list' else {}
  print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
 except Exception:pass
""")
    form('mcp',{'name':'fixture-mcp','scope':'global','command':'python3','argsText':str(mcp_file)},type='local')
    until(lambda:any(m['name']=='fixture-mcp' and m['status']=='connected' for m in api('/mcp')),'MCP did not connect')
    manage('mcp',action='disable',name='fixture-mcp',scope='global')
    until(lambda:any(m['name']=='fixture-mcp' and m['status']=='disabled' for m in api('/mcp')),'MCP did not disable')
    manage('mcp',action='enable',name='fixture-mcp',scope='global')
    until(lambda:any(m['name']=='fixture-mcp' and m['status']=='connected' for m in api('/mcp')),'MCP did not re-enable')
    manage('mcp',action='restart',name='fixture-mcp',scope='global')
    # PKCE through the public management adapter, including a remote pasted callback.
    import urllib.parse
    class OAuth(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def respond(self,status,value=None,extra=None):
            data=b'' if value is None else json.dumps(value).encode();self.send_response(status)
            self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)))
            for k,v in (extra or {}).items():self.send_header(k,v)
            self.end_headers();self.wfile.write(data)
        def do_GET(self):
            origin='http://127.0.0.1:'+str(self.server.server_port)
            if 'oauth-protected-resource' in self.path:self.respond(200,{'resource':origin+'/mcp','authorization_servers':[origin],'scopes_supported':['mcp']})
            elif '.well-known' in self.path:self.respond(200,{'issuer':origin,'authorization_endpoint':origin+'/authorize','token_endpoint':origin+'/token','code_challenge_methods_supported':['S256'],'scopes_supported':['mcp']})
            else:self.respond(405)
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0')))
            if self.path=='/token':
                data=urllib.parse.parse_qs(body.decode());assert data.get('code')==['fixture-code'] and data.get('code_verifier');self.respond(200,{'access_token':'fixture-access-token','token_type':'Bearer','expires_in':3600,'scope':'mcp'});return
            if self.headers.get('Authorization')!='Bearer fixture-access-token':self.respond(401,extra={'WWW-Authenticate':'Bearer resource_metadata="http://127.0.0.1:'+str(self.server.server_port)+'/.well-known/oauth-protected-resource", scope="mcp"'});return
            message=json.loads(body)
            if 'id' not in message:self.respond(202);return
            result={'protocolVersion':message.get('params',{}).get('protocolVersion','2024-11-05'),'capabilities':{'tools':{}},'serverInfo':{'name':'secure-fixture','version':'1'}} if message['method']=='initialize' else {'tools':[]}
            self.respond(200,{'jsonrpc':'2.0','id':message['id'],'result':result})
    oauth=http.server.ThreadingHTTPServer(('127.0.0.1',0),OAuth);threading.Thread(target=oauth.serve_forever,daemon=True).start()
    try:
        form('mcp',{'name':'oauth-fixture','scope':'global','url':'http://127.0.0.1:'+str(oauth.server_port)+'/mcp','oauthClientId':'fixture-client','oauthScopesText':'mcp'},type='remote')
        until(lambda:any(m['name']=='oauth-fixture' and m['status']=='needs_auth' for m in api('/mcp')),'OAuth did not request authorization')
        login=manage('mcp',action='login',name='oauth-fixture',scope='global');loginid=next(e['request']['id'] for e in login['entries'] if e['label']=='Check status')
        assert login['data']['state']=='pending' and login['data']['id']==loginid;auth=urllib.parse.urlparse(login['data']['url']);assert login['lines'][0]==login['data']['url'];params=urllib.parse.parse_qs(auth.query);assert params['code_challenge_method']==['S256']
        callback=params['redirect_uri'][0]+'?code=fixture-code&state='+params['state'][0]
        callbackform=manage('mcp',action='login_callback_form',id=loginid);assert callbackform['fields'][0]['secret']
        manage('mcp',action='login_callback',id=loginid,callback=callback)
        until(lambda:any(m['name']=='oauth-fixture' and m['status']=='connected' for m in api('/mcp')),'Remote callback OAuth did not connect')
        status=manage('mcp',action='login_status',id=loginid);assert status['data']['state']=='connected' and 'is connected' in '\n'.join(status['lines']),status
        manage('mcp',action='logout',name='oauth-fixture',scope='global')
        login=manage('mcp',action='login',name='oauth-fixture',scope='global');cancel=next(e['request'] for e in login['entries'] if e['label']=='Cancel sign-in');cancelled=api('/manage/mcp',cancel);assert cancelled['title']=='Sign-in cancelled' and cancelled['data']['state']=='cancelled'
        try:manage('mcp',action='login_callback',id=cancel['id'],callback=callback);raise AssertionError('Cancelled OAuth accepted callback')
        except urllib.error.HTTPError:pass
        manage('mcp',action='remove',name='oauth-fixture',scope='global')
    finally:oauth.shutdown();oauth.server_close()
    # Disabled native schedule, explicit run, and durable run history.
    form('schedules',{'name':'fixture-schedule','prompt':'scheduled parity marker','triggerType':'interval','triggerValue':'3600','timezone':''})
    schedule=next(e['request'] for e in manage('schedules',action='view')['entries'] if e['request']['action']=='inspect')
    detail=api('/manage/schedules',schedule);assert 'disabled' in '\n'.join(detail['lines'])
    manage('schedules',action='run',id=schedule['id'])
    history=until(lambda:manage('schedules',action='history',id=schedule['id'])['lines'],'Schedule did not record a run',90)
    assert json.loads(history[0])['status']=='success', history
    # Native TUI, keyboard routing, form persistence and snapshot submission.
    args=[str(binary),'--workspace',str(workspace),'--trust-project']
    run(['tmux','new-session','-d','-x','150','-y','42','-s',name,e2e_tmux.pane_command(env,args)])
    until(lambda:'Echo 1' in capture(),'TUI did not start')
    command('/reasoning');until(lambda:'provider default' in capture(),'Reasoning picker missing');assert 'xhigh' not in capture();evidence('reasoning-model-domain');keys('Escape')
    command('/agents');until(lambda:'Select agent' in capture(),'Agent picker missing');screen=capture();assert 'compaction' not in screen and 'Fixture worker' not in screen;evidence('primary-agents');keys('Escape')
    command('/memory');until(lambda:'Project memory' in capture(),'Memory manager missing');choose('Create memory');until(lambda:'Memory name' in capture(),'Memory form missing')
    fill('Memory name','tmux-memory');fill('Description','created from terminal');fill('Content','terminal memory content');choose('Save')
    until(lambda:any('tmux-memory'==e['label'] for e in manage('memory',action='view')['entries']),'TUI did not persist memory');evidence('memory-created');keys('Escape')
    literal('unsent parity draft');keys('C-s');until(lambda:(config/'tui-stash.json').exists() and 'unsent parity draft' in (config/'tui-stash.json').read_text(),'Stash shortcut did not save');assert 'unsent parity draft' not in capture()
    command('/stash');until(lambda:'unsent parity draft' in capture(),'Stash did not retain draft');choose('unsent parity draft');choose('Restore prompt');keys('Escape');until(lambda:'unsent parity draft' in capture(),'Stash restore missing');evidence('stash-restored');keys('C-u')
    literal('@sample.txt:2-3');until(lambda:'sample.txt:2-3' in capture(),'Range completion missing');keys('Tab');until(lambda:'[@sample.txt:2-3]' in capture(),'File snapshot not attached');evidence('file-range-reference');keys('Enter')
    until(lambda:any('line two\nline three' in json.dumps(api('/threads/'+t['id']+'/items')).replace('\\n','\n') for t in api('/threads')),'File snapshot not submitted')
    keys('F6');until(lambda:'Session usage and context' in capture(),'Configured usage shortcut failed');assert 'Cache read tokens' in capture();evidence('usage-shortcut');keys('Escape')
    command('/models');until(lambda:'Select model' in capture(),'Model picker missing');keys('C-f');evidence('model-favorite');keys('Escape')
    command('/providers');until(lambda:'Local provider fixture' in capture(),'Provider manager missing');choose('Local provider fixture');choose('API key');choose('API key');literal('never-show-this-key');until(lambda:'••••' in capture(),'Credential input did not render');assert 'never-show-this-key' not in capture();evidence('credential-masked');keys('Escape','Escape')
    command('/mcp');until(lambda:'fixture-mcp' in capture(),'MCP manager missing');choose('fixture-mcp');until(lambda:'Restart' in capture(),'MCP actions missing');evidence('mcp-actions');keys('Escape')
    literal('@worker');until(lambda:'Delegate to agent' in capture(),'Agent mention missing');keys('Tab');until(lambda:'[@worker]' in capture(),'Agent context missing');evidence('agent-reference');keys('C-s')
    run(['tmux','kill-session','-t',name]);run(['tmux','new-session','-d','-x','150','-y','42','-s',name,e2e_tmux.pane_command(env,args)])
    until(lambda:'Echo 1' in capture(),'TUI did not reopen')
    command('/stash');until(lambda:'@worker' in capture() and 'Delegate the relevant work' not in capture(),'Stash did not survive restart');evidence('stash-after-restart');keys('Escape')
    command('/models');until(lambda:'Select model' in capture(),'Model picker missing after restart');assert '★' in capture();evidence('favorites-after-restart');keys('Escape')
    manage('mcp',action='remove',name='fixture-mcp',scope='global')
    until(lambda:not any(m['name']=='fixture-mcp' for m in api('/mcp')),'Removed MCP remained configured')

    # Invoke a real tool after hook hot reload, proving the created handler runs.
    thread=api('/threads',{'provider':'echo','model':'echo-1','agent':'build'})
    api('/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':'!tool bash {"command":"printf checked > tool-hit.txt"}','attachments':[]})
    until(lambda:(workspace/'hook-hit.txt').exists(),'Created hook did not execute')
    assert (workspace/'tool-hit.txt').read_text()=='checked'
    # Edit/remove the actual provider source without touching its credential store.
    details=manage('providers',action='inspect',name='fixture-ui');editrequest=next(e['request'] for e in details['entries'] if e['label']=='Edit global provider')
    editview=api('/manage/providers',editrequest);change={**editview['submit'],**{f['key']:f['value'] for f in editview['fields']},'label':'Edited fixture'}
    api('/manage/providers',change)
    try:api('/manage/providers',change);raise AssertionError('Stale provider edit accepted')
    except urllib.error.HTTPError:pass
    form('providers',{'name':'project-fixture','scope':'project','label':'Project fixture','url':f'http://127.0.0.1:{service.server_port}/v1','protocol':'openai-chat-completions','models':'fixture-model','requiresAuth':'false'})
    until(lambda:any(p['id']=='project-fixture' for p in api('/providers')),'Project provider missing')
    details=manage('providers',action='inspect',name='project-fixture');remove=next(e['request'] for e in details['entries'] if e['label']=='Remove project provider');api('/manage/providers',remove)
    until(lambda:not any(p['id']=='project-fixture' for p in api('/providers')),'Project provider removal failed')
    # Restore the captured configuration after later edits, using its reviewed target list.
    preview=manage('profiles',action='restore_preview',id=profile['id']);assert any(str(config) in line for line in preview['lines'])
    api('/manage/profiles',preview['entries'][0]['request'])
    until(lambda:not any(p['id']=='fixture-ui' for p in api('/providers')),'Profile restore did not replace configuration')
    evidence('final')
    (root/'result.json').write_text(json.dumps({'passed':True,'server':endpoint['url'],'workspace':str(workspace),'checks':['authentication','memory CRUD/conflict','hooks execution','profiles capture/restore','scoped custom provider edits/removal/conflicts','PKCE remote callback/cancel','custom provider/credential','native schedule execution','model reasoning','agent filtering','TUI forms','stash','range snapshots','keybindings','usage','favorites']},indent=2))
    print('PASS parity API + tmux.',flush=True)
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    if endpoint:subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=35)
    service.shutdown();service.server_close()
    e2e_scratch.settle(root)
