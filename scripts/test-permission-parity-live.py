#!/usr/bin/env python3
"""Real-model skill/helper/extension proof, with isolated state and persisted records."""
import json,os,pathlib,subprocess,tempfile,time,urllib.request,uuid
import e2e_scratch
root=pathlib.Path(tempfile.mkdtemp(prefix='haarness-parity-live-')).resolve();ws=root/'workspace';ws.mkdir();home=root/'home';config=home/'config';config.mkdir(parents=True)
binary=str(pathlib.Path(os.environ.get('HAARNESS_BIN','target/debug/haarness')).resolve())
env={k:v for k,v in os.environ.items() if not k.startswith('HAARNESS_')};env.update(HAARNESS_HOME=str(home),HAARNESS_DATA_DIR=str(root/'data'))
if not env.get('OPENAI_API_KEY'):
 auth=json.loads((pathlib.Path.home()/'.local/share/haarness/auth.json').read_text());env['OPENAI_API_KEY']=auth['openai']['key']
model=os.environ.get('HAARNESS_PARITY_MODEL','gpt-5.6-luna')
# --data-dir uses its own config plane; explicitly write there too for shared discovery.
skill=root/'data/config/skills/parity-context';skill.mkdir(parents=True)
(skill/'SKILL.md').write_text('---\nname: parity-context\ndescription: Retrieve the parity fixture token and helper path\n---\nDynamic token: !`printf LIVE_DYNAMIC_PROOF`\nUse the read tool to read '+str(skill/'helper.txt')+'. Then call ext__parity__greet with the helper token, write both tokens to proof.txt using bash with timeout 0, and read proof.txt.\n')
(skill/'helper.txt').write_text('LIVE_HELPER_PROOF')
extension=root/'parity.ts';extension.write_text("export default {apiVersion:1,name:'parity',tools:{greet:{description:'Return the provided parity token',parameters:{type:'object',properties:{token:{type:'string'}},required:['token']},execute({token}){return 'LIVE_EXTENSION_PROOF '+token}}}}")
(root/'data/config/extensions.json').write_text(json.dumps({'enabled':True,'tool_timeout_ms':0,'entries':[{'source':str(extension)}]}))
(ws/'.haarness').mkdir();(ws/'.haarness/config.toml').write_text('[defaults]\nprovider="openai"\nmodel="'+model+'"\n[permissions]\nbash="ask"\n[execution]\nshell_timeout_ms=0\ncode_timeout_ms=0\ncode_max_calls=0\nsubagent_depth=2\nsubagent_concurrency=1\n')
c=None
try:
 p=subprocess.run([binary,'--workspace',str(ws),'desktop'],input='{"action":"connect"}',env=env,text=True,capture_output=True,check=True,timeout=60);c=json.loads(p.stdout)
 def api(path,value=None):
  req=urllib.request.Request(c['url']+'/v1'+path,data=None if value is None else json.dumps(value).encode(),headers={'Authorization':'Bearer '+c.get('token',''),'Content-Type':'application/json'})
  with urllib.request.urlopen(req,timeout=30) as reply:
   data=reply.read();return json.loads(data) if data else None
 skills=api('/skills');assert any(s['name']=='parity-context' for s in skills),[s['name'] for s in skills]
 thread=api('/threads',{'title':'Live permission parity proof','provider':'openai','model':model})
 turn=api('/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':'Use the installed parity-context skill now by calling skill. Follow its instructions using the named native tools. Read its helper file, call ext__parity__greet, then use bash with timeout 0 to write the dynamic token, helper token, and extension result to proof.txt, and use read to verify the file. Do not use any network or unrelated files. Complete this small fixture autonomously after any tool approval.'})
 approvals=[];deadline=time.monotonic()+240
 while time.monotonic()<deadline:
  for pending in api('/permissions'):
   assert pending['permission']=='bash',pending
   command=pending.get('detail',{}).get('command','')
   assert 'LIVE_' in command or 'proof.txt' in command,command
   approvals.append({'permission':pending['permission'],'operation':pending['operation']})
   api('/permissions/'+pending['permissionId'],{'decision':'once'})
  current=api('/turns/'+turn['id'])
  if current['status'] not in ['running','queued']:break
  time.sleep(.25)
 else:raise AssertionError('Live model timed out')
 items=api('/threads/'+thread['id']+'/items');(root/'items.json').write_text(json.dumps(items,indent=2));(root/'approvals.json').write_text(json.dumps(approvals,indent=2))
 assert current['status']=='completed',current
 results=[i['data'] for i in items if i['data']['type']=='tool_result']
 for name in ['skill','read','ext__parity__greet','bash']:assert any(r['name']==name and not r['is_error'] for r in results),(name,results)
 text=(ws/'proof.txt').read_text()
 for token in ['LIVE_DYNAMIC_PROOF','LIVE_HELPER_PROOF','LIVE_EXTENSION_PROOF']:assert token in text,text
 assert approvals
 summary={'model':model,'toolResults':[{'name':r['name'],'error':r['is_error']} for r in results],'approvals':len(approvals),'proof':text}
 (root/'results.json').write_text(json.dumps(summary,indent=2));print('PASS real-model skill, dynamic context approval, installed helper, extension, shell timeout 0, and read-back:',root,flush=True)
finally:
 subprocess.run([binary,'--workspace',str(ws),'serve','--stop'],env=env,capture_output=True,timeout=30)
 e2e_scratch.settle(root)
