"""MCP stdio fixture that reports what its environment holds: the `env` tool answers with the value of the
variable its `name` argument names, or `<unset>`."""
import json
import os
import sys

for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'env-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'env', 'description': 'Report an environment variable.',
                             'inputSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}}, 'required': ['name']}}]}
    elif method == 'tools/call':
        name = request['params'].get('arguments', {}).get('name', '')
        result = {'content': [{'type': 'text', 'text': os.environ.get(name, '<unset>')}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
