#!/usr/bin/env python3
"""Opt-in unified credentials against a running local Cloud, router and native runtime.

HAARNESS_KEY_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080
HAARNESS_E2E_OAUTH_FILE=/private/oauth.json (access_token field)
Creates/revokes its own keys, deletes its own workspace, and makes small billable model calls.
Never prints credentials. Run with the SDK Python environment after generating both SDKs.
"""
import asyncio,json,os,time,subprocess
from pathlib import Path
from datetime import datetime,timezone,timedelta
from uuid import uuid4
import httpx
from haarness_sdk import HaarnessCloud,AsyncHaarnessCloud,ApiError

BASE=os.environ['HAARNESS_CLOUD_API_URL'].rstrip('/')
assert os.environ.get('HAARNESS_KEY_E2E')=='1' and httpx.URL(BASE).host in ['127.0.0.1','localhost']
MODEL=os.getenv('HAARNESS_KEY_E2E_MODEL','openai/gpt-4.1-nano')
SCOPES=['models:read','models:invoke','workspaces:read','workspaces:write','workspaces:agent','schedules:read','schedules:write','marketplace:read','billing:read']
EMPTY='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
admin=HaarnessCloud(BASE,token=json.loads(Path(os.environ['HAARNESS_E2E_OAUTH_FILE']).read_text())['access_token'])
keys=[]; workspace=None; cloud=None

def proof(label): print('PASS:',label,flush=True)
def idem():return {'headers':{'Idempotency-Key':uuid4().hex}}
def create(scopes=SCOPES,seconds=3600):
    row=admin.create_account_api_key({'label':'SDK unified '+uuid4().hex[:8],'scopes':scopes,'expiresAt':(datetime.now(timezone.utc)+timedelta(seconds=seconds)).isoformat(),'policy':{'modelAllowlist':[MODEL],'monthlyBudgetCredits':'100','perRequestMaxCredits':'10','maxConcurrency':3,'ratePerMinute':300}},options=idem());keys.append(row['key']['id']);return row

def denied(fn,status):
    try:fn()
    except ApiError as e:assert e.status==status,(e.status,status)
    else:raise AssertionError('Operation should be denied')
def until(fn,predicate,seconds=120):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        value=fn()
        if predicate(value):return value
        time.sleep(.4)
    raise AssertionError('Timed out waiting for expected state')

def model(token,url,model_id=MODEL):
    return httpx.post(url+'/chat/completions',headers={'Authorization':'Bearer '+token},json={'model':model_id,'messages':[{'role':'user','content':'Reply with KEY_MODEL_OK only.'}],'max_tokens':32},timeout=60)
try:
    row=create(); token=row['secretKey']; kid=row['key']['id']; router=row['routerUrl'].rstrip('/')
    cloud=HaarnessCloud(BASE,token=token)
    assert cloud.api_key_context()['key']['id']==kid
    cloud.list_workspaces();cloud.list_cloud_agents();cloud.list_cloud_schedules();cloud.get_wallet()
    denied(lambda:cloud.list_account_api_keys(),403)
    denied(lambda:cloud.billing_portal(),403)
    reply=model(token,router);assert reply.status_code==200,reply.text
    assert 'KEY_MODEL_OK' in reply.json()['choices'][0]['message']['content']
    assert reply.headers.get('x-haarness-cost-nanos')
    assert model(token,router,'openai/gpt-4.1-mini').status_code==403
    proof('one key: Cloud, real inference, allowlist and denied administration')
    workspace=cloud.new_workspace({'title':'Unified key SDK workspace','configuration':{'model':'router/'+MODEL,'diskGb':1},'start':True},options=idem())
    until(workspace.get,lambda s:s['observedState']=='running')
    first=workspace.chats.create({'title':'Python sync'}); second=workspace.chats.create({'title':'Python async'})
    runtime=workspace.connect(); runtime.write_file({'path':'unified-key-proof.txt','content':'UNIFIED_SHARED_FILES_OK','revision':EMPTY})
    result=first.ask('Reply SYNC_KEY_CHAT_OK. Do not use tools.',{'tools':{}})
    assert result['turn']['status']=='completed'
    async def async_checks():
        async with AsyncHaarnessCloud(BASE,token=token) as client:
            assert (await client.api_key_context())['key']['id']==kid
            shared=client.workspace(workspace.id); r=await shared.connect()
            assert (await r.read_file({'path':'unified-key-proof.txt'}))['content']=='UNIFIED_SHARED_FILES_OK'
            chats=await shared.chats.list();assert len(chats)==2
            result=await shared.chats.get(second.id).ask('Reply ASYNC_KEY_CHAT_OK. Do not use tools.',{'tools':{}})
            assert result['turn']['status']=='completed'
    asyncio.run(async_checks())
    proof('sync and async SDKs: shared workspace, isolated chats, uploads and model replies')
    env=dict(os.environ,HAARNESS_API_KEY=token,HAARNESS_KEY_E2E_WORKSPACE=workspace.id)
    subprocess.run(['node',str(Path(__file__).with_name('api_keys_ts_e2e.mjs'))],env=env,check=True)
    schedule=cloud.create_cloud_schedule({'workspaceId':workspace.id,'chatId':first.id,'name':'Unified SDK key schedule','prompt':'Reply KEY_SCHEDULE_OK','at':(datetime.now(timezone.utc)+timedelta(hours=1)).isoformat()},options=idem())
    run=cloud.run_cloud_schedule(schedule['id'],options=idem())
    until(lambda:cloud.cloud_schedule_runs(schedule['id']),lambda p:any(r['id']==run['id'] and r['status']=='completed' for r in p['items']))
    cloud.delete_cloud_schedule(schedule['id']);proof('Cloud schedule creation, execution, history and deletion using the same key')
    ticket=cloud.attach_workspace(workspace.id)
    admin.update_account_api_key(kid,{'scopes':['models:read']})
    denied(lambda:cloud.list_workspaces(),403)
    denied(lambda:cloud.attach_workspace(workspace.id),403)
    assert model(token,router).status_code==403
    until(lambda:httpx.get(ticket['gatewayUrl'].rstrip('/')+'/v1/info',headers={'Authorization':'Bearer '+ticket['ticket']},timeout=15).status_code,lambda status:status in (401,403),20)
    proof('permission removal denies Cloud and inference and invalidates existing runtime tickets')
    admin.update_account_api_key(kid,{'scopes':SCOPES,'policy':{'modelAllowlist':[MODEL],'monthlyBudgetCredits':'0','ratePerMinute':300,'maxConcurrency':3}})
    rotated=admin.rotate_account_api_key(kid,options=idem());new=rotated['secretKey']
    assert rotated['key']['id']==kid
    assert model(token,router).status_code==401
    denied(lambda:cloud.api_key_context(),401)
    assert model(new,router).status_code==402
    admin.revoke_account_api_key(kid)
    assert model(new,router).status_code==401
    with HaarnessCloud(BASE,token=new) as revoked:denied(lambda:revoked.api_key_context(),401)
    proof('rotation, retained zero budget and revocation across both services')
    short=create(['models:read','models:invoke'],seconds=2)
    time.sleep(2.2)
    with HaarnessCloud(BASE,token=short['secretKey']) as expired:denied(lambda:expired.api_key_context(),401)
    assert model(short['secretKey'],router).status_code==401
    proof('real expiration denies both services')
finally:
    if workspace:
        workspace.close();admin.delete_workspace(workspace.id,options=idem())
    if cloud:cloud.close()
    for key in keys:admin.revoke_account_api_key(key)
    admin.close()
