import json
import os
import sys
import time

if 'LAUNCH_LOG' in os.environ:
    with open(os.environ['LAUNCH_LOG'], 'a') as file:
        file.write('launched\n')

for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request.get('method')
    if method == 'initialize':
        time.sleep(float(os.environ.get('INIT_DELAY', '0')))
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'recovery-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}} for name in ['ping', 'crash', 'wait']]}
    elif method == 'tools/call':
        if request['params']['name'] == 'crash':
            with open(os.environ['CALL_LOG'], 'a') as file:
                file.write('called\n')
            sys.exit(0)
        if request['params']['name'] == 'wait':
            # A call that takes a while, then says it finished.
            time.sleep(float(request['params'].get('arguments', {}).get('seconds', 1)))
            result = {'content': [{'type': 'text', 'text': 'waited'}], 'isError': False}
        else:
            result = {'content': [{'type': 'text', 'text': 'pong'}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
