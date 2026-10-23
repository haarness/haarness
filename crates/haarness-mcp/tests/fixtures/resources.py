"""MCP stdio fixture that declares resources: a text page, an image, a template, and a page that never answers.

A read of `docs://slow` gets no answer; the cancellation the client sends for it is appended to CANCEL_LOG.
"""
import base64
import json
import os
import sys

PNG = base64.b64encode(
    bytes.fromhex(
        '89504e470d0a1a0a0000000d4948445200000001000000010806000000'
        '1f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082'
    )
).decode()

RESOURCES = [
    {'uri': 'docs://setup', 'name': 'setup', 'title': 'Setup guide', 'mimeType': 'text/markdown'},
    {'uri': 'docs://logo', 'name': 'logo', 'mimeType': 'image/png'},
]


def reply(request_id, result):
    print(json.dumps({'jsonrpc': '2.0', 'id': request_id, 'result': result}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'notifications/cancelled':
        if 'CANCEL_LOG' in os.environ:
            with open(os.environ['CANCEL_LOG'], 'a') as file:
                file.write('cancelled %s\n' % request['params'].get('requestId'))
        continue
    if 'id' not in request:
        continue
    if method == 'initialize':
        reply(request['id'], {
            'protocolVersion': '2024-11-05',
            'capabilities': {'tools': {}, 'resources': {}},
            'serverInfo': {'name': 'resources-fixture', 'version': '1'},
        })
    elif method == 'tools/list':
        reply(request['id'], {'tools': []})
    elif method == 'resources/list':
        reply(request['id'], {'resources': RESOURCES})
    elif method == 'resources/templates/list':
        reply(request['id'], {'resourceTemplates': [{'uriTemplate': 'docs://page/{id}', 'name': 'page'}]})
    elif method == 'resources/read':
        uri = request['params']['uri']
        if uri == 'docs://slow':
            continue
        if uri == 'docs://setup':
            contents = [{'uri': uri, 'mimeType': 'text/markdown', 'text': '# Setup\nRun the fixture.'}]
        elif uri == 'docs://logo':
            contents = [{'uri': uri, 'mimeType': 'image/png', 'blob': PNG}]
        elif uri.startswith('docs://page/'):
            contents = [{'uri': uri, 'mimeType': 'text/plain', 'text': 'Page ' + uri.rsplit('/', 1)[1]}]
        else:
            print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32002, 'message': 'Resource not found'}}), flush=True)
            continue
        reply(request['id'], {'contents': contents})
    else:
        reply(request['id'], {})
