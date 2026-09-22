#!/usr/bin/env python3
"""Real HTTP admission, concurrency and daemon restart proof for pending inputs."""
import json, sys, time, urllib.request, urllib.error
from pathlib import Path
from uuid import uuid4
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'sdks'/'tests'))
from server import Fixture

def until(check):
    deadline=time.monotonic()+20
    while time.monotonic()<deadline:
        value=check()
        if value:return value
        time.sleep(.05)
    raise AssertionError('Server did not reach the expected state')

with Fixture() as server:
    def api(path,body=None,expected=200,auth=True):
        headers={'Content-Type':'application/json'}
        if auth:headers['Authorization']='Bearer '+server.token
        request=urllib.request.Request(server.url+'/v1'+path,data=None if body is None else json.dumps(body).encode(),headers=headers)
        try:
            with urllib.request.urlopen(request,timeout=15) as response:
                status=response.status;raw=response.read();result=json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            status=error.code;result=error.read().decode()
        assert status==expected,(path,status,expected,result)
        return result
    thread=api('/threads',{'title':'Queue restart proof','provider':'echo','model':'echo-1'},201)['id']
    other=api('/threads',{'title':'Other queue','provider':'echo','model':'echo-1'},201)['id']
    original=api(f'/threads/{thread}/turns',{'requestId':str(uuid4()),'text':'!tool bash {"command":"printf restart-original"}'},202)
    until(lambda:api('/permissions'))
    key=str(uuid4())
    request={'requestId':key,'delivery':'queue','text':'survives hard restart'}
    api(f'/threads/{thread}/input',request,202)
    queue=api(f'/threads/{thread}/input/queue')
    assert len(queue['items'])==1
    assert not api(f'/threads/{other}/input/queue')['items']
    api(f'/threads/{thread}/input/queue',expected=401,auth=False)
    api(f'/threads/{other}/input/queue',{'action':'remove','id':key,'revision':0},409)
    api(f'/threads/{thread}/input/queue',{'action':'reorder','revision':queue['revision'],'ids':[key,key]},400)
    api(f'/threads/{thread}/input',{'requestId':str(uuid4()),'delivery':'queue','text':''},400)
    # Kill this owned fixture at an arbitrary active point; startup must not
    # replay tool execution or silently run the deferred work.
    server.process.kill();server.process.wait(timeout=10);server.process.stdout.close();server.log.close();server.start()
    assert not api('/turns/active')
    restored=api(f'/threads/{thread}/input/queue')
    assert restored==queue
    api(f'/threads/{thread}/input',{'requestId':key,'text':'changed retry'},202)
    assert not api('/turns/active')
    assert api(f'/threads/{thread}/input/queue')==queue
    api(f'/threads/{thread}/input/queue',{'action':'steer','revision':queue['revision'],'id':key})
    until(lambda:not api('/turns/active'))
    assert not api(f'/threads/{thread}/input/queue')['items']
    items=api(f'/threads/{thread}/items')
    assert sum(item['data'].get('text')=='survives hard restart' for item in items)==1
    assert any(item['data']['type']=='assistant_message' and 'survives hard restart' in item['data'].get('text','') for item in items)
    api(f'/threads/{thread}/input',request,202)
    assert not api('/turns/active')
    assert not api(f'/threads/{thread}/input/queue')['items']
    print('PASS: HTTP queue isolation, auth, validation, hard restart, parked retry, explicit resume and exactly-once admission')
