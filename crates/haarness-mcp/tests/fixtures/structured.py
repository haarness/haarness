"""MCP stdio fixture whose tools answer with structured content and little or no text."""
import json
import sys

WEATHER = {'temperature': 21, 'unit': 'C', 'proof': 'structured-only-proof'}
CHART = {'points': 3, 'proof': 'structured-beside-image-proof'}
PIXEL = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='
TOOLS = {
    # Declares an output schema and answers with structured content only.
    'weather': {'content': [], 'structuredContent': WEATHER},
    # Only an image beside its structured content.
    'chart': {'content': [{'type': 'image', 'data': PIXEL, 'mimeType': 'image/png'}], 'structuredContent': CHART},
    # A failure that says nothing.
    'broken': {'content': [], 'isError': True},
    # A failure that says why in structured content only.
    'refused': {'content': [], 'structuredContent': {'code': 'E42-refused-proof'}, 'isError': True},
}

for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'structured-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'},
                             **({'outputSchema': {'type': 'object'}} if 'structuredContent' in answer and not answer.get('isError') else {})}
                            for name, answer in TOOLS.items()]}
    elif method == 'tools/call':
        result = TOOLS[request['params']['name']]
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
