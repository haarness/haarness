#!/usr/bin/env python3
"""Model refresh through a real local server and tmux-driven terminal renderer."""
import http.server,json,os,pathlib,shlex,subprocess,tempfile,threading,time
import e2e_scratch
import e2e_tmux
binary=pathlib.Path(os.environ.get('HAARNESS_BIN','target/release/haarness')).resolve()
root=pathlib.Path(tempfile.mkdtemp(prefix='haarness-models-tui-')).resolve()
workspace=root/'workspace'
(workspace/'.haarness').mkdir(parents=True)
models=[{'id':'gpt-6-astra'}]
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_GET(self):
        body=json.dumps({'data':models}).encode()
        self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(body)
service=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
threading.Thread(target=service.serve_forever,daemon=True).start()
(workspace/'.haarness/config.toml').write_text('[defaults]\nprovider="catalog-test"\nmodel="gpt-6-astra"\n[providers.catalog-test]\nname="Catalog fixture"\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:'+str(service.server_port)+'/v1"\nrequires_auth=false\n[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n')
name='haarness-models-'+str(os.getpid())
env={**os.environ,'HAARNESS_DATA_DIR':str(root/'data')}
for key in ['HAARNESS_TEST_PROVIDER','OPENAI_API_KEY','ANTHROPIC_API_KEY','GEMINI_API_KEY','GOOGLE_API_KEY']:
    env.pop(key,None)
def run(args,**kwargs): return subprocess.run(args,check=True,text=True,capture_output=True,**kwargs).stdout
def capture(pane=name+':0.0'): return run(['tmux','capture-pane','-p','-t',pane])
def keys(*values,pane=name+':0.0'): run(['tmux','send-keys','-t',pane,*values])
def command(value,pane=name+':0.0'):
    keys('C-u',pane=pane);run(['tmux','send-keys','-l','-t',pane,value]);keys('Enter',pane=pane)
def until(predicate,label):
    until=time.monotonic()+30
    while time.monotonic()<until:
        if predicate(): return
        time.sleep(.1)
    raise AssertionError(label+'\n'+capture())
endpoint=None
try:
    endpoint=json.loads(run([str(binary),'--workspace',str(workspace),'--trust-project','desktop'],env=env,input='{"action":"connect"}',timeout=90))
    args=[str(binary),'--workspace',str(workspace),'--trust-project']
    run(['tmux','new-session','-d','-x','150','-y','38','-s',name,e2e_tmux.pane_command(env,args)])
    until(lambda:'GPT-6 Astra' in capture(),'Friendly name not rendered in composer')
    command('/models')
    until(lambda:'Select model' in capture(),'Picker not opened')
    screen=capture()
    assert 'GPT-6 Astra' in screen and 'claude-opus' not in screen
    (root/'initial.txt').write_text(screen)
    keys('Escape')
    models[:]=[{'id':'future-deployment','display_name':'Future Reasoner'}]
    time.sleep(1.1)
    command('/models')
    until(lambda:'Future Reasoner' in capture(),'Picker did not refresh provider catalog')
    screen=capture()
    assert 'catalog-test/gpt-6-astra' not in screen
    (root/'refreshed.txt').write_text(screen)
    keys('Enter')
    until(lambda:'Future Reasoner' in capture() and 'Select model' not in capture(),'New model was not selected')
    # A second terminal on the same config folder, started before either
    # saves: each favorites and picks a model, and neither undoes the other.
    models[:]=[{'id':'gpt-6-astra'},{'id':'future-deployment','display_name':'Future Reasoner'}]
    time.sleep(1.1)
    second=name+':1.0'
    run(['tmux','new-window','-t',name+':1',e2e_tmux.pane_command(env,args)])
    until(lambda:'Ask anything' in capture(second),'Second TUI did not open')
    for pane,word,label in ((name+':0.0','astra','GPT-6 Astra'),(second,'future','Future Reasoner')):
        command('/models',pane=pane)
        until(lambda:'Select model' in capture(pane) and 'GPT-6 Astra' in capture(pane),'Picker not opened in '+pane)
        run(['tmux','send-keys','-l','-t',pane,word])
        keys('C-f',pane=pane)
        until(lambda:'★ '+label in capture(pane),'Favorite not shown in '+pane)
        keys('Enter',pane=pane)
        until(lambda:'Select model' not in capture(pane),'Model not picked in '+pane)
    state=(root/'data'/'config'/'tui-state.toml').read_text()
    (root/'two-terminals-state.toml').write_text(state)
    for key in ('model_favorites','model_recents'):
        line=next(line for line in state.splitlines() if line.startswith(key))
        assert 'catalog-test/gpt-6-astra' in line and 'catalog-test/future-deployment' in line,state
    run(['tmux','kill-window','-t',name+':1'])
    run(['tmux','respawn-pane','-k','-t',name+':0.0',e2e_tmux.pane_command(env,args)])
    until(lambda:'Reasoner' in capture() or 'Astra' in capture(),'TUI did not reopen')
    command('/models')
    until(lambda:'★ GPT-6 Astra' in capture() and '★ Future Reasoner' in capture(),'Both favorites not kept after a restart')
    (root/'two-terminals.txt').write_text(capture())
    keys('Escape')
    models.clear();keys('Escape');time.sleep(1.1);command('/models')
    until(lambda:'Welcome to Haarness' in capture() and 'Haarness needs an AI model' in capture(),'Empty catalog did not offer provider setup')
    (root/'empty.txt').write_text(capture())
    print('PASS real TUI: friendly name, connected-only picker, refresh, selection, two terminals keep both favorites and recents, empty catalog.',flush=True)
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    if endpoint:
        subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=30)
    service.shutdown();service.server_close()
    e2e_scratch.settle(root)
