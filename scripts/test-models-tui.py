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
def capture(): return run(['tmux','capture-pane','-p','-t',name+':0.0'])
def keys(*values): run(['tmux','send-keys','-t',name+':0.0',*values])
def command(value):
    keys('C-u');run(['tmux','send-keys','-l','-t',name+':0.0',value]);keys('Enter')
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
    models.clear();keys('Escape');time.sleep(1.1);command('/models')
    until(lambda:'Welcome to Haarness' in capture() and 'Haarness needs an AI model' in capture(),'Empty catalog did not offer provider setup')
    (root/'empty.txt').write_text(capture())
    print('PASS real TUI: friendly name, connected-only picker, refresh, selection, empty catalog.',flush=True)
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    if endpoint:
        subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=30)
    service.shutdown();service.server_close()
    e2e_scratch.settle(root)
