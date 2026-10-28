"""MCP stdio fixture whose tools are named by `TOOLS` (a JSON list of names). A call answers with JSON naming
the server (`LABEL`), the tool name it was called with and the folder the server runs in."""
import json
import os
import sys

TOOLS = json.loads(os.environ.get('TOOLS', '["echo"]'))

for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-11-25', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'names', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}} for name in TOOLS]}
    elif method == 'tools/call':
        answer = {'server': os.environ.get('LABEL', ''), 'tool': request['params']['name'], 'cwd': os.getcwd()}
        result = {'content': [{'type': 'text', 'text': json.dumps(answer)}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
