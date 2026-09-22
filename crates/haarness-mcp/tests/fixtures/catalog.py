import json, os, sys
changed = False
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion':'2025-11-25','capabilities':{'tools':{'listChanged':True}},'serverInfo':{'name':'catalog','version':'1'}}
    elif method == 'tools/list':
        if changed and os.environ.get('FAIL_REFRESH') == '1':
            print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32603,'message':'fixture list failure'}}), flush=True)
            continue
        result = {'tools':[{'name':name,'description':name,'inputSchema':{'type':'object'}} for name in (['refresh','new_tool'] if changed else ['refresh','old_tool'])]}
    elif method == 'tools/call':
        changed = True
        print(json.dumps({'jsonrpc':'2.0','method':'notifications/tools/list_changed'}), flush=True)
        result = {'content':[{'type':'text','text':'refreshed'}]}
    else: result = {}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
