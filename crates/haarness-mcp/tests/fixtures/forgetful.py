"""Streamable HTTP MCP fixture that keeps its sessions in memory, as servers built on the TypeScript SDK's examples do:
started again, it has forgotten them, and a request naming one gets a 400, as JSON that names no request (`json`) or
as text (`text`). Each `initialize`, refusal and tool call is logged, a line each.

Usage: forgetful.py PORT json|text LOG
"""
import http.server
import json
import sys
import uuid

port, mode, log = int(sys.argv[1]), sys.argv[2], sys.argv[3]
sessions = set()


def note(line):
    with open(log, 'a') as file:
        file.write(line + '\n')


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def send(self, status, body=b'', kind=None, session=None):
        self.send_response(status)
        if kind:
            self.send_header('Content-Type', kind)
        if session:
            self.send_header('Mcp-Session-Id', session)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def reply(self, id, result, session=None):
        self.send(200, json.dumps({'jsonrpc': '2.0', 'id': id, 'result': result}).encode(), 'application/json', session)

    def do_GET(self):
        self.send(405)

    def do_DELETE(self):
        self.send(405)

    def do_POST(self):
        message = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = message.get('method')
        if method == 'initialize':
            session = uuid.uuid4().hex
            sessions.add(session)
            note('initialize')
            return self.reply(message['id'], {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}},
                                              'serverInfo': {'name': 'forgetful-' + mode, 'version': '1'}}, session)
        if self.headers.get('Mcp-Session-Id') not in sessions:
            note('refused')
            if mode == 'json':
                refusal = {'jsonrpc': '2.0', 'error': {'code': -32000, 'message': 'Bad Request: No valid session ID provided'},
                           'id': None}
                return self.send(400, json.dumps(refusal).encode(), 'application/json')
            return self.send(400, b'Bad Request: Server not initialized', 'text/plain')
        if 'id' not in message:
            return self.send(202)
        if method == 'tools/list':
            return self.reply(message['id'], {'tools': [{'name': 'echo', 'description': 'Says which server answered',
                                                         'inputSchema': {'type': 'object'}}]})
        if method == 'tools/call':
            note('call')
            return self.reply(message['id'], {'content': [{'type': 'text', 'text': f'forgetful-{mode} answered'}]})
        self.send(200, json.dumps({'jsonrpc': '2.0', 'id': message['id'],
                                   'error': {'code': -32601, 'message': 'Method not found'}}).encode(), 'application/json')


http.server.ThreadingHTTPServer(('127.0.0.1', port), Handler).serve_forever()
