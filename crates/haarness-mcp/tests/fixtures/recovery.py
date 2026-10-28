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
        delay = os.environ.get('INIT_DELAY', '0')
        # A file naming the delay, read at each launch, so a test can change it between starts.
        if os.path.exists(os.environ.get('INIT_DELAY_FILE', '')):
            with open(os.environ['INIT_DELAY_FILE']) as file:
                delay = file.read()
        time.sleep(float(delay))
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'recovery-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}} for name in ['ping', 'crash', 'wait']]}
    elif method == 'tools/call':
        if request['params']['name'] == 'crash':
            with open(os.environ['CALL_LOG'], 'a') as file:
                file.write('called\n')
            sys.exit(0)
        if request['params']['name'] == 'wait':
            # A call that takes a while, then says (and logs) it finished.
            time.sleep(float(request['params'].get('arguments', {}).get('seconds', 1)))
            with open(os.environ['CALL_LOG'], 'a') as file:
                file.write('waited\n')
            result = {'content': [{'type': 'text', 'text': 'waited'}], 'isError': False}
        else:
            result = {'content': [{'type': 'text', 'text': 'pong'}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
