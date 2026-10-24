"""MCP stdio fixture that offers prompts and never answers a prompt request, while its `ping` tool answers at once.

The cancellation the client sends for an unanswered request is appended to CANCEL_LOG.
"""
import json
import os
import sys

for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'notifications/cancelled':
        if 'CANCEL_LOG' in os.environ:
            with open(os.environ['CANCEL_LOG'], 'a') as file:
                file.write('cancelled %s\n' % request['params'].get('requestId'))
        continue
    if 'id' not in request or method in ('prompts/list', 'prompts/get'):
        continue
    if method == 'initialize':
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}, 'prompts': {}},
                  'serverInfo': {'name': 'prompts-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'ping', 'description': 'Answer pong.', 'inputSchema': {'type': 'object'}}]}
    elif method == 'tools/call':
        result = {'content': [{'type': 'text', 'text': 'pong'}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
