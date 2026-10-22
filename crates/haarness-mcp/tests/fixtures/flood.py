"""MCP stdio fixture whose replies can pass Haarness's 64 MiB message limit.

`flood` starts its reply and writes FLOOD_MIB MiB (default 80) without ever ending the line, then waits, as a
server stuck in a loop would. `big` answers with BIG_MIB MiB (default 20) of text in one ordinary message.
`ping` answers `pong`. With FLOOD_INIT=1 the server floods instead of answering `initialize`. Each launch
appends a line to LAUNCH_LOG and its process id to PID_LOG when they are set.
"""
import json
import os
import sys
import time

for name, value in (('LAUNCH_LOG', 'launched'), ('PID_LOG', str(os.getpid()))):
    if name in os.environ:
        with open(os.environ[name], 'a') as file:
            file.write(value + '\n')


def flood(request_id):
    out = sys.stdout.buffer
    out.write(('{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"' % json.dumps(request_id)).encode())
    chunk = b'x' * (1 << 20)
    try:
        for _ in range(int(os.environ.get('FLOOD_MIB', '80'))):
            out.write(chunk)
        out.flush()
    except BrokenPipeError:
        pass
    time.sleep(3600)


for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request.get('method')
    if method == 'initialize':
        if os.environ.get('FLOOD_INIT') == '1':
            flood(request['id'])
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'flood-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}} for name in ['ping', 'flood', 'big']]}
    elif method == 'tools/call':
        tool = request['params']['name']
        if tool == 'flood':
            flood(request['id'])
        text = 'z' * (int(os.environ.get('BIG_MIB', '20')) << 20) if tool == 'big' else 'pong'
        result = {'content': [{'type': 'text', 'text': text}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
