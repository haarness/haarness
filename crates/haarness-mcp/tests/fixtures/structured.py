"""MCP stdio fixture whose tools answer with structured content and little or no text, or embed resources as
base64 blobs (an image under a type in capitals, text, an archive), or answer with 200 KB of text."""
import base64
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
    # Resources embedded as blobs: an image, text and an archive.
    'embedded': {'content': [
        {'type': 'text', 'text': 'Three files.'},
        {'type': 'resource', 'resource': {'uri': 'ui://chart.png', 'mimeType': 'IMAGE/PNG; charset=binary', 'blob': PIXEL}},
        {'type': 'resource', 'resource': {'uri': 'data://row.json', 'mimeType': 'application/json',
                                          'blob': base64.b64encode(b'{"proof": "embedded-text-proof"}').decode()}},
        {'type': 'resource', 'resource': {'uri': 'files://pack.zip', 'mimeType': 'application/zip',
                                          'blob': base64.b64encode(bytes(300)).decode()}},
    ]},
    # Audio of a type Haarness does not carry, beside an image it does.
    'odd_media': {'content': [{'type': 'audio', 'data': 'GkXfo0AAAA==', 'mimeType': 'audio/webm'},
                              {'type': 'image', 'data': PIXEL, 'mimeType': 'image/png'}]},
    # 120 KB of JSON on one line, as JSON.stringify gives it.
    'rows': {'content': [{'type': 'text', 'text': json.dumps([{'id': n, 'title': 'row %d' % n, 'body': 'b' * 40}
                                                             for n in range(1500)], separators=(',', ':'))}]},
    # 66 KB of one-line JSON holding what a parser would change: an integer past 64 bits, a
    # decimal past 17 digits, a key given twice.
    'ledger': {'content': [{'type': 'text', 'text': '[%s]' % ','.join(
        ['{"wei":1234567890123456789012345,"rate":0.12345678901234567890123,"key":1,"key":2}'] * 800)}]},
    # The same JSON after a heading line.
    'found': {'content': [{'type': 'text', 'text': 'Found 800 entries:\n[%s]' % ','.join(
        ['{"wei":1234567890123456789012345,"key":1,"key":2}'] * 1600)}]},
    # 52 KB of JSON indented four spaces, as Python's json.dumps(rows, indent=4) writes it: 48 KB at two.
    'spaced': {'content': [{'type': 'text', 'text': json.dumps(
        [{'id': n, 'title': 't' * 50, 'body': 'b' * 50, 'owner': 'o' * 50, 'state': 'open'} for n in range(200)],
        indent=4)}]},
    # 60 KB of arrays nested 30,000 deep.
    'nested': {'content': [{'type': 'text', 'text': '[' * 30000 + ']' * 30000}]},
    # Structured content that names `truncated` itself, beside 80 KB of text.
    'tree': {'content': [{'type': 'text', 'text': 't' * 80 * 1024}],
             'structuredContent': {'tree': [{'path': 'a'}], 'truncated': False}},
    # A failure that says why at length.
    'loud_failure': {'content': [{'type': 'text', 'text': 'f' * 200 * 1024}], 'isError': True},
    # 200 KB of text, as a tool that returns a whole table does.
    'wide': {'content': [{'type': 'text', 'text': ''.join('row %05d %s\n' % (n, 'w' * 90) for n in range(2000))}],
             'structuredContent': {'rows': 2000}},
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
