#!/usr/bin/env python3
"""Real attached TUI coverage for repository consent, dynamic context, and nested tasks."""
import json,os,pathlib,shlex,subprocess,tempfile,time,urllib.request,uuid
import e2e_scratch
from e2e_env import clean_env
root=pathlib.Path(tempfile.mkdtemp(prefix='haarness-parity-tmux-')).resolve()
workspace=root/'primary';linked=root/'linked';workspace.mkdir()
config=workspace/'.haarness';(config/'commands').mkdir(parents=True)
# Chats start in Auto; these approvals wait for a person, so the project (and its linked worktree) asks for Review.
(config/'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n[permissions]\nbash="ask"\n[execution]\nshell_timeout_ms=0\nsubagent_depth=2\nsubagent_concurrency=1\n')
(config/'commands/context.md').write_text('---\ndescription: Dynamic context\n---\nContext: !`printf TMUX_CONTEXT_PROOF`\n')
def git(*args):subprocess.run(['git','-C',str(workspace),*args],check=True,capture_output=True)
git('init','-b','main');git('add','.');git('-c','user.name=Fixture','-c','user.email=fixture@example.test','commit','-m','fixture');git('worktree','add','-b','linked',str(linked))
binary=str(pathlib.Path(os.environ.get('HAARNESS_BIN','target/debug/haarness')).resolve())
env=clean_env();env.update(HAARNESS_HOME=str(root/'home'),HAARNESS_DATA_DIR=str(root/'data'),HAARNESS_TEST_PROVIDER='1',HAARNESS_SERVER_TOKEN='',HAARNESS_SERVER_PASSWORD='')
name='haarness-parity-'+uuid.uuid4().hex[:10];connections=[];checks=[]
def cli(ws,*args,input=None):return subprocess.run([binary,'--workspace',str(ws),*args],input=input,env=env,text=True,capture_output=True,check=True,timeout=60).stdout
def connect(ws):
 c=json.loads(cli(ws,'desktop',input='{"action":"connect"}'));connections.append((ws,c));return c
def api(c,path,body=None):
 req=urllib.request.Request(c['url']+'/v1'+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+c.get('token',''),'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=10) as reply:
  data=reply.read();return json.loads(data) if data else None
def tmux(*args):return subprocess.run(['tmux',*args],text=True,capture_output=True,check=True,timeout=10).stdout
def capture():return tmux('capture-pane','-p','-t',name+':0.0')
def until(fn,label):
 end=time.monotonic()+30
 while time.monotonic()<end:
  value=fn()
  if value:return value
  time.sleep(.1)
 raise AssertionError(label+'\n'+capture())
def keys(*args):tmux('send-keys','-t',name,*args)
def check(label):
 checks.append(label);(root/(str(len(checks))+'.txt')).write_text(capture());print('PASS '+label,flush=True)
try:
 c=connect(workspace);env['HAARNESS_SERVER_TOKEN']=c.get('token','');thread=api(c,'/threads',{'title':'Permission parity TUI','provider':'echo','model':'echo-1'})
 command=shlex.join([binary,'--workspace',str(root),'attach',c['url'],'--thread',thread['id']])
 launch='env '+ ' '.join(k+'='+shlex.quote(env[k]) for k in ['HAARNESS_HOME','HAARNESS_DATA_DIR','HAARNESS_TEST_PROVIDER','HAARNESS_SERVER_TOKEN','HAARNESS_SERVER_PASSWORD'])+' '+command
 tmux('new-session','-d','-s',name,'-x','150','-y','44',launch)
 until(lambda:'Echo 1' in capture(),'TUI attach')
 def turn(text,command=None):
  return api(c,'/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':text,**({'command':command} if command else {})})
 def finish(t):
  result=until(lambda:(value if (value:=api(c,'/turns/'+t['id']))['status'] not in ['running','queued'] else None),'turn completion')
  assert result['status']=='completed',result
 def approval():
  until(lambda:api(c,'/permissions'),'pending permission');until(lambda:'Allow once' in capture(),'approval visible')
 t=turn('!tool bash {"command":"git status --short"}');approval();until(lambda:'Repository' in capture(),'repository choice');keys('r');until(lambda:'worktrees' in capture(),'repository confirmation');check('repository scope and patterns are visible before confirmation');keys('Enter');finish(t)
 linked_c=connect(linked);linked_thread=api(linked_c,'/threads',{'title':'Linked consent','provider':'echo','model':'echo-1'})
 linked_turn=api(linked_c,'/threads/'+linked_thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':'!tool bash {"command":"git status --porcelain"}'})
 until(lambda:api(linked_c,'/turns/'+linked_turn['id'])['status']=='completed','linked grant');assert not api(linked_c,'/permissions');check('repository consent applies to a separately running linked-worktree daemon')
 t=turn('',command='context');approval();keys('y');finish(t);until(lambda:'TMUX_CONTEXT_PROOF' in capture(),'dynamic context result');check('dynamic context receives a normal TUI approval and completes')
 leaf='!tool bash '+json.dumps({'command':'printf TMUX_NESTED_PROOF > nested.txt'})
 child='!tool task '+json.dumps({'agent':'general','prompt':leaf})
 t=turn('!tool task '+json.dumps({'agent':'general','prompt':child}));approval();check('grandchild approval appears in the root TUI with one execution slot');keys('y');finish(t);assert (workspace/'nested.txt').read_text()=='TMUX_NESTED_PROOF';check('nested tasks finish without capacity deadlock')
 (root/'items.json').write_text(json.dumps(api(c,'/threads/'+thread['id']+'/items'),indent=2));(root/'results.json').write_text(json.dumps(checks,indent=2))
 print('PASS',len(checks),'new TUI scenarios.',flush=True)
finally:
 subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
 for ws,_ in connections:
  try:cli(ws,'serve','--stop')
  except Exception:pass
 e2e_scratch.settle(root)
