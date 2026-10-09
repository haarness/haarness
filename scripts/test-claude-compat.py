#!/usr/bin/env python3
"""Real server/runtime regression gate for explicitly installed plugin bundles.
Uses Echo for deterministic tool requests; writes only within an isolated workspace.
Pass --real-plugins to also install and customize Anthropic's public plugin-dev.
"""
import json, os, pathlib, subprocess, tempfile, urllib.request, urllib.error, sys
repo=pathlib.Path(__file__).resolve().parents[1]
root=pathlib.Path(tempfile.mkdtemp(prefix='haarness-claude-compat-'))
workspace=root/'workspace';workspace.mkdir();state=root/'state';home=root/'home';home.mkdir()
env={k:v for k,v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(HAARNESS_DATA_DIR=str(state),HAARNESS_HOME=str(home),HOME=str(home),HAARNESS_TEST_PROVIDER='1',HAARNESS_DEFAULT_PROVIDER='echo',HAARNESS_DEFAULT_MODEL='echo-1')
binary=os.environ.get('HAARNESS_BIN',str(repo/'target/debug/haarness'))
results={};connection={}
def write(path,content):path.parent.mkdir(parents=True,exist_ok=True);path.write_text(content)
def cli(*args,timeout=60):
 p=subprocess.run([binary,'--workspace',str(workspace),'--trust-project',*args],env=env,text=True,capture_output=True,timeout=timeout)
 return {'code':p.returncode,'out':p.stdout,'err':p.stderr}
def api(path,body=None):
 data=None if body is None else json.dumps(body).encode()
 req=urllib.request.Request(connection['url']+'/v1'+path,data=data,headers={'Authorization':'Bearer '+connection['token'],'Content-Type':'application/json'})
 try:
  with urllib.request.urlopen(req,timeout=30) as response:return json.load(response)
 except urllib.error.HTTPError as e:return {'http':e.code,'error':e.read().decode()}
def check(name,condition,detail=None):
 results[name]={'passed':bool(condition),'detail':detail};(root/'results.json').write_text(json.dumps(results,indent=2))
 assert condition, f'{name}: {detail}'
 print('PASS',name,flush=True)
def run(text,*options):return cli('run',text,'--provider','echo','--model','echo-1',*options)
plugins=state/'config/plugins/local'
for name in ['a','b','custom-paths']:
 write(plugins/name/'.claude-plugin/plugin.json',json.dumps({'name':name,'version':'1.0.0'}))
for name in ['a','b']:
 write(plugins/name/'skills/shared/SKILL.md',f'---\nname: shared\ndescription: {name} skill\n---\nBODY_{name}\n')
for name,front in [('restricted','tools: Read, Grep, Glob'),('array','tools:\n  - Read\n  - Grep'),('denied','disallowedTools: Write, Edit'),('inherit','model: inherit')]:
 write(plugins/'a/agents'/f'{name}.md',f'---\ndescription: Audit agent\n{front}\n---\nAudit instructions.\n')
write(plugins/'a/skills/manual/SKILL.md','---\nname: manual\ndescription: MANUAL_SECRET_DESCRIPTION\ndisable-model-invocation: true\n---\nMANUAL_SECRET_BODY\n')
write(plugins/'a/skills/headless/SKILL.md','---\nname: headless\ndescription: Background skill\nuser-invocable: false\n---\nHEADLESS_BODY\n')
write(plugins/'a/skills/forked/SKILL.md','---\nname: forked\ndescription: Research skill\ncontext: fork\nagent: Explore\nmodel: inherit\n---\nFORK_BODY $ARGUMENTS\n')
write(plugins/'a/commands/context.md','---\ndescription: Dynamic context\n---\nLIVE: !`printf DYNAMIC_OK`\nFIRST=$ARGUMENTS[0] SECOND=$1 ZERO=$0\nROOT=${CLAUDE_PLUGIN_ROOT}\n')
write(plugins/'a/commands/injection.md','---\ndescription: Safe substitutions\n---\n$ARGUMENTS\n')
write(plugins/'a/skills/dynamic/SKILL.md','---\nname: dynamic\ndescription: Dynamic skill\n---\nSKILL_LIVE !`printf SKILL_DYNAMIC_OK`\n')
write(plugins/'a/skills/readonly/SKILL.md','---\nname: readonly\ndescription: Restricted skill\ndisallowed-tools: Write, Edit\n---\n$ARGUMENTS\n')
write(plugins/'a/commands/named.md','---\narguments: [file, target, missing]\n---\nNAMED $file to $target ($missing) MISSING $5\nPROJECT ${CLAUDE_PROJECT_DIR}\n')
write(plugins/'a/commands/failed-context.md','---\ndescription: Failed context\n---\nNEVER_RENDER !`exit 2`\n')
write(workspace/'.haarness/skills/standalone/SKILL.md','---\nname: standalone\ndescription: Local skill\n---\nSTANDALONE_BODY $ARGUMENTS\n')
write(plugins/'custom-paths/.claude-plugin/plugin.json',json.dumps({'name':'custom-paths','commands':['./extra/check.md'],'agents':['./extra/reader.md'],'skills':['./extra/research']}))
write(plugins/'custom-paths/extra/check.md','---\ndescription: Custom command\n---\nCUSTOM_COMMAND\n')
write(plugins/'custom-paths/extra/reader.md','---\nname: reader\ndescription: Custom agent\n---\nCUSTOM_AGENT\n')
write(plugins/'custom-paths/extra/research/SKILL.md','---\nname: custom-research\ndescription: Custom skill\n---\nCUSTOM_SKILL\n')
try:
 if '--real-plugins' in sys.argv:
  check('public marketplace',cli('plugin','marketplace','add','https://github.com/anthropics/claude-code.git',timeout=120)['code']==0)
  check('public plugin install',cli('plugin','install','plugin-dev@claude-code-plugins',timeout=120)['code']==0)
 p=subprocess.run([binary,'--workspace',str(workspace),'--trust-project','desktop'],input=json.dumps({'action':'connect'}),env=env,text=True,capture_output=True,timeout=60)
 connection=json.loads(p.stdout)
 commands={c['name']:c for c in api('/commands')};skills={s['name']:s for s in api('/skills')}
 for name in ['restricted','array','denied']:
  result=run(f'!tool write {{"path":"{name}-breach.txt","content":"bad"}}','--agent','a:'+name)
  check('agent '+name+' rejects write',not (workspace/f'{name}-breach.txt').exists() and ('disabled' in result['out']+result['err'] or 'denied' in result['out']+result['err']),result)
 check('standalone slash', 'standalone' in commands and 'STANDALONE_BODY hello' in run('hello','--command','standalone')['out'])
 check('model-only hidden from slash','a:headless' not in commands)
 manual=run('!tool skill {"name":"a:manual"}')
 check('manual-only rejected by model tool','MANUAL_SECRET_BODY' not in manual['out'] and 'only' in manual['out']+manual['err'],manual)
 check('manual-only user invocation','MANUAL_SECRET_BODY' in run('','--command','a:manual')['out'])
 check('qualified skill identity',all(n in skills for n in ['a:shared','b:shared']))
 check('no ambiguous bare skill',api('/skills/shared').get('http')==404)
 for name in ['a','b']:
  check(name+' skill content',f'BODY_{name}' in api('/skills/'+name+':shared').get('instructions',''))
 check('manifest command path','custom-paths:check' in commands)
 check('manifest agent path',cli('agent','get','custom-paths:reader')['code']==0)
 check('manifest skill path','custom-paths:custom-research' in skills)
 result=run('alpha beta','--command','a:context')
 check('dynamic and indexed expansion','LIVE: DYNAMIC_OK' in result['out'] and 'FIRST=alpha SECOND=beta ZERO=alpha' in result['out'] and str(plugins/'a') in result['out'],result)
 result=run('!`touch argument-injection`','--command','a:injection')
 check('arguments cannot introduce shell snippets',not (workspace/'argument-injection').exists() and '!`touch' in result['out'],result)
 result=run('!tool skill {"name":"a:dynamic"}')
 check('model skill dynamic context','SKILL_DYNAMIC_OK' in result['out'] and '!`' not in result['out'],result)
 before_readonly={t['id'] for t in api('/threads')}
 result=run('!tool write {"path":"skill-breach.txt","content":"bad"}','--command','a:readonly')
 check('skill disallowed tools enforced',not (workspace/'skill-breach.txt').exists() and 'disabled' in result['out']+result['err'],result)
 readonly_thread=next(t['id'] for t in api('/threads') if t['id'] not in before_readonly and not t.get('parentId'))
 result=run('!tool write {"path":"next-turn.txt","content":"allowed"}','--thread',readonly_thread)
 check('skill restrictions expire next message',(workspace/'next-turn.txt').exists(),result)
 result=run('"two words" last','--command','a:named')
 check('named missing and project substitutions','NAMED two words to last () MISSING $5' in result['out'] and str(workspace) in result['out'],result)
 result=run('','--command','a:failed-context')
 check('failed context aborts invocation',result['code']!=0 and 'NEVER_RENDER' not in result['out'],result)
 result=run('!tool write {"path":"unknown-breach.txt","content":"bad"}','--agent','a:missing')
 check('unknown agents fail closed',result['code']!=0 and not (workspace/'unknown-breach.txt').exists(),result)
 before={t['id'] for t in api('/threads')};result=run('research','--command','a:forked')
 children=[t for t in api('/threads') if t['id'] not in before and t.get('parentId')]
 check('forked skill creates explore child',len(children)==1 and children[0]['agent']=='explore' and 'FORK_BODY research' in result['out'],{'result':result,'children':children})
 for name in ['a:inherit']+(['plugin-dev:plugin-validator'] if '--real-plugins' in sys.argv else []):
  response=api('/manage/assets',{'kind':'agent','action':'customize','name':name,'scope':'local','description':'Edited inherited model','prompt':'Edited instructions'})
  check(name+' customize',not response.get('http'),response)
  check(name+' run after edit',run('MODEL_OK','--agent',name)['code']==0)
 if '--real-plugins' in sys.argv:
  check('manifest-free public plugin skills qualified','plugin-dev:plugin-structure' in skills)
  response=api('/manage/assets',{'kind':'skill','action':'customize','name':'Plugin Structure','scope':'local','description':'Edited structure','instructions':'EDITED_PUBLIC_SKILL'})
  check('public legacy skill customize',not response.get('http'),response)
 print('Haarness plugin compatibility E2E passed. Evidence:',root/'results.json',flush=True)
finally:
 print('Server cleanup:',cli('serve','--stop')['code'],flush=True)
 print('Evidence retained in',root,flush=True)
