#!/usr/bin/env python3
"""Opt-in live model discovery + streamed turns through a private Haarness server.

Requires OPENAI_API_KEY and ANTHROPIC_API_KEY in the environment. Sends only
synthetic prompts and a temporary fixture file. Never writes credentials to disk.
"""
import json, os, pathlib, queue, re, secrets, subprocess, tempfile, threading, time, urllib.request, uuid

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/release/haarness')).resolve()
for name in ['OPENAI_API_KEY', 'ANTHROPIC_API_KEY']:
    if not os.environ.get(name):
        raise SystemExit('Set '+name+' to run the opt-in live test')
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-models-live-')).resolve()
workspace = root/'workspace'
(workspace/'.haarness').mkdir(parents=True)
(workspace/'proof.txt').write_text('The verification marker is harbor-528.\n')
(workspace/'.haarness/config.toml').write_text('[defaults]\nprovider="openai"\nmodel="gpt-6-astra"\n[permissions]\nread="allow"\n[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n')
token=secrets.token_hex(24)
env={**os.environ, 'HAARNESS_DATA_DIR':str(root/'data'), 'HAARNESS_SERVER_TOKEN':token}
env.pop('HAARNESS_TEST_PROVIDER',None)
ready=queue.Queue()
log=(root/'server.log').open('w')
process=subprocess.Popen([str(binary),'--workspace',str(workspace),'--trust-project','serve','--port','0'],env=env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=log,text=True)
def reader():
    for line in process.stdout:
        found=re.search(r'Haarness server listening on (http://\S+)',line)
        if found: ready.put(found[1])
    ready.put(None)
threading.Thread(target=reader,daemon=True).start()
checks=[]
def check(label):
    checks.append(label)
    print('PASS '+label,flush=True)
def api(path,value=None,method=None):
    request=urllib.request.Request(url+path,data=None if value is None else json.dumps(value).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},method=method)
    with urllib.request.urlopen(request,timeout=30) as response:
        body=response.read()
        return json.loads(body) if body else None
try:
    url=ready.get(timeout=90)
    assert url, 'Private server failed to start'
    catalog=api('/v1/providers/refresh',{},'POST')
    selected={p['id']:p for p in catalog if p['id'] in ['openai','anthropic']}
    for provider in selected.values():
        assert provider['authenticated'] and provider['models']
        assert len({model['id'] for model in provider['models']}) == len(provider['models'])
        check(provider['id']+' live catalog: '+str(len(provider['models']))+' unique models')
    a=next(model for model in selected['openai']['models'] if model['id']=='gpt-6-astra')
    c=next(model for model in selected['anthropic']['models'] if model['id']=='claude-fable-5-1')
    assert a['displayName']=='GPT-6 Astra' and a['contextWindow']==1050000
    assert c['displayName']=='Claude Fable 5.1' and c['contextWindow']==1000000
    assert c['maxOutputTokens']==128000 and 'xhigh' in c['reasoningEfforts']
    with urllib.request.urlopen(urllib.request.Request('https://api.anthropic.com/v1/models?limit=1000',headers={'x-api-key':os.environ['ANTHROPIC_API_KEY'],'anthropic-version':'2023-06-01'}),timeout=30) as response:
        raw=json.load(response)
    assert not raw.get('has_more'), 'Live test comparison needs pagination'
    assert {m['id'] for m in selected['anthropic']['models']} == {m['id'] for m in raw['data']}
    (root/'catalog.json').write_text(json.dumps(selected,indent=2))
    check('live membership, provider names/limits/efforts and static metadata fallback')
    cases=[('openai','gpt-6-astra','low'),('openai','gpt-5.6-sol','none'),('openai','gpt-5.6-terra','low'),('openai','gpt-5.6-luna','low'),('anthropic','claude-fable-5-1','high'),('anthropic','claude-opus-5','low'),('anthropic','claude-sonnet-5','low')]
    for provider,model,effort in cases:
        assert any(m['id']==model for m in selected[provider]['models']), model+' absent from account catalog'
        thread=api('/v1/threads',{'title':'Model verification '+model,'provider':provider,'model':model})
        tools = model in ['gpt-6-astra','claude-fable-5-1']
        prompt = ('Read proof.txt using the read tool. Reply with only the verification marker found in that file.' if tools else 'Reply with exactly: harbor-528')
        turn=api('/v1/threads/'+thread['id']+'/turns',{'requestId':str(uuid.uuid4()),'text':prompt,'reasoningEffort':effort})
        deadline=time.monotonic()+120
        while time.monotonic()<deadline:
            result=api('/v1/turns/'+turn['id'])
            if result['status'] in ['completed','failed','cancelled','interrupted']: break
            pending=api('/v1/permissions')
            assert not pending, 'Unexpected tool permission in read-only fixture'
            time.sleep(.2)
        assert result['status']=='completed', model+' turn did not complete: '+result['status']
        items=api('/v1/threads/'+thread['id']+'/items')
        text='\n'.join(item['data'].get('text','') for item in items if item['data']['type']=='assistant_message')
        assert 'harbor-528' in text, model+' response did not contain proof'
        if tools: assert any(item['data']['type']=='tool_call' and item['data'].get('name','').split('.')[-1]=='read' for item in items), model+' did not exercise tool execution'
        saved=api('/v1/threads/'+thread['id'])
        assert saved['provider']==provider and saved['model']==model
        (root/(model+'-items.json')).write_text(json.dumps(items,indent=2))
        check(model+' streamed turn, '+effort+' reasoning'+(', read tool round trip' if tools else ''))
    check('refresh remains available after live turns')
    assert len(api('/v1/providers/refresh',{},'POST'))==len(catalog)
    print(json.dumps({'checks':len(checks),'evidence':str(root)}),flush=True)
finally:
    process.terminate()
    try: process.wait(timeout=15)
    except subprocess.TimeoutExpired: process.kill();process.wait()
    log.close()
