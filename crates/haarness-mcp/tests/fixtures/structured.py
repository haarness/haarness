"""MCP stdio fixture whose tools answer with structured content and little or no text, or embed resources as
base64 blobs (an image under a type in capitals, text, an archive), or answer with 200 KB of text, or with the
same text each call and the call's number as structured content, or with screenshots larger than providers take, or
with content that is not valid MCP, or with nothing."""
import base64
import json
import struct
import sys
import zlib


def png(width, height, level):
    """A PNG of `width`×`height` RGB pixels, a gradient, its data compressed at zlib `level` (0 stores it as it is)."""
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    row = bytearray(3 * width)
    row[0::3] = bytes(x % 256 for x in range(width))
    row[2::3] = b'\x80' * width
    rows = []
    for y in range(height):
        row[1::3] = bytes([y % 256]) * width
        rows.append(b'\x00' + bytes(row))
    header = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(b''.join(rows), level))
            + chunk(b'IEND', b''))


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
    # The same text as `spaced` each call, with how many calls it has had as its structured content.
    'stamp': {'content': [], 'structuredContent': {'call': 0}},
    # 80 KB of one-digit values three levels down, four and a half times as long laid out in full.
    'series': {'content': [{'type': 'text', 'text': json.dumps(
        {'result': {'series': [n % 10 for n in range(40000)]}}, separators=(',', ':'))}]},
    # 60 KB of arrays nested 30,000 deep.
    'nested': {'content': [{'type': 'text', 'text': '[' * 30000 + ']' * 30000}]},
    # Structured content that names `truncated` itself, beside 80 KB of text.
    'tree': {'content': [{'type': 'text', 'text': 't' * 80 * 1024}],
             'structuredContent': {'tree': [{'path': 'a'}], 'truncated': False}},
    # A failure that says why at length.
    'loud_failure': {'content': [{'type': 'text', 'text': 'f' * 200 * 1024}], 'isError': True},
    # A message that went out, answered with an image that gives no type and a block of a type MCP does not define.
    'unshaped': {'content': [{'type': 'text', 'text': 'Message sent.'}, {'type': 'image', 'data': PIXEL},
                             {'type': 'chart', 'points': [1, 2]}]},
    # Nothing at all, as a REST API's 204 becomes.
    'emptied': {},
    # 200 KB of text, as a tool that returns a whole table does.
    'wide': {'content': [{'type': 'text', 'text': ''.join('row %05d %s\n' % (n, 'w' * 90) for n in range(2000))}],
             'structuredContent': {'rows': 2000}},
}

# Screenshots, made when called: a 4K one stored uncompressed (24 MB, past the 10 MiB a result attaches), and a
# full page far taller than providers take (small, compressed).
SCREENSHOTS = {'screenshot': (3840, 2160, 0), 'full_page': (1440, 9000, 1)}
for name in SCREENSHOTS:
    TOOLS[name] = {'content': []}

calls = 0
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
        if request['params']['name'] == 'stamp':
            calls += 1
            result = {'content': TOOLS['spaced']['content'], 'structuredContent': {'call': calls}}
    else:
        result = {}
    if method == 'tools/call' and request['params']['name'] in SCREENSHOTS:
        width, height, level = SCREENSHOTS[request['params']['name']]
        data = base64.b64encode(png(width, height, level)).decode()
        result = {'content': [{'type': 'text', 'text': 'The whole page.'},
                              {'type': 'image', 'data': data, 'mimeType': 'image/png'}]}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
