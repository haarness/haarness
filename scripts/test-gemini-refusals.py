#!/usr/bin/env python3
"""A real daemon against a mock Gemini API that refuses what Google's refuses (the real Gemini adapter, SSE).

Proves, end to end:
- an MCP server whose tools carry JSON Schema shapes Gemini answers with a 400 (a `required` name the
  schema does not declare, a draft-04 boolean `exclusiveMinimum`, a draft-07 tuple, a `$ref` loop through
  required properties) no longer fails every turn: the tools reach the model in a shape Gemini takes, the
  model calls one, and its result comes back;
- a day's free quota used up, which Google names with the same short wait as a minute's ("Please retry
  in 11.6s") and tells apart only by the quota's id, fails the turn at once, without the waits, in words
  that say it comes back at midnight Pacific time;
- a minute's quota with that wait is still sat out, and the turn answers.
"""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
EVIDENCE = Path(os.environ.get('HAARNESS_GEMINI_EVIDENCE', REPO / 'target/gemini-refusals/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
MODEL = 'gemini-3.8-flash'
REQUESTS = []
MINUTE_REFUSED = []

# An MCP stdio server with one tool per shape Gemini refuses.
MCP_SERVER = r'''
import json, sys
TOOLS = [
    {"name": "lookup", "description": "Look up a word in the project glossary.",
     "inputSchema": {"type": "object", "properties": {"word": {"type": "string"}}, "required": ["word", "language"]}},
    {"name": "between", "description": "Check a number against a range.",
     "inputSchema": {"type": "object", "properties": {"value": {"type": "number", "minimum": 0, "exclusiveMinimum": True}}}},
    {"name": "pair", "description": "Store a name and a count.",
     "inputSchema": {"type": "object", "properties": {"pair": {"type": "array", "items": [{"type": "string"}, {"type": "integer"}], "additionalItems": False}}}},
    {"name": "outline", "description": "Save a nested outline.",
     "inputSchema": {"type": "object", "properties": {"root": {"$ref": "#/$defs/Node"}}, "required": ["root"],
                     "$defs": {"Node": {"type": "object", "properties": {"title": {"type": "string"}, "child": {"$ref": "#/$defs/Node"}}, "required": ["title", "child"]}}}},
]
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "glossary", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        word = request.get("params", {}).get("arguments", {}).get("word")
        result = {"content": [{"type": "text", "text": f"GLOSSARY_PROOF: {word} is the blue kettle"}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
'''


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


def refused_shape(schema, path='', root=None):
    """Google's words for the first shape of `schema` that its function declarations refuse, or None."""
    root = schema if root is None else root
    if isinstance(schema, list):
        for index, item in enumerate(schema):
            found = refused_shape(item, f'{path}[{index}]', root)
            if found:
                return found
        return None
    if not isinstance(schema, dict):
        return None
    where = path or 'top-level'
    properties = schema.get('properties') if isinstance(schema.get('properties'), dict) else {}
    if schema.get('type') == 'object' or 'properties' in schema:
        for name in schema.get('required', []):
            if name not in properties:
                return f"schema at {where} requires unspecified property '{name}'"
    for key in ('exclusiveMinimum', 'exclusiveMaximum'):
        if isinstance(schema.get(key), bool):
            return f'value at {path}.{key} must be a number'.lstrip('.')
    if isinstance(schema.get('items'), list):
        return f'schema at {path}.items must be a boolean or an object'.lstrip('.')
    # A definition that must hold itself through required properties.
    for name, definition in (root.get('$defs') or {}).items() if schema is root else []:
        for prop in definition.get('required', []):
            if (definition.get('properties') or {}).get(prop, {}).get('$ref') == f'#/$defs/{name}':
                return ('ref loops are only supported if they include optional or nullable property values, or a '
                        f'potentially-zero-length array items, but a ref loop of required fields was found at '
                        f'$defs.{name}.properties.{prop}')
    for key, value in schema.items():
        if key in ('enum', 'const', 'default', 'examples'):
            continue
        if key in ('properties', '$defs') and isinstance(value, dict):
            for name, child in value.items():
                found = refused_shape(child, f'{path}.{key}.{name}'.lstrip('.'), root)
                if found:
                    return found
        elif isinstance(value, (dict, list)):
            found = refused_shape(value, f'{path}.{key}'.lstrip('.'), root)
            if found:
                return found
    return None


def quota_refusal(quota_id, wait='11.615098148'):
    return json.dumps({'error': {
        'code': 429,
        'message': ('You exceeded your current quota, please check your plan and billing details. For more '
                    'information on this error, head to: https://ai.google.dev/gemini-api/docs/rate-limits. To '
                    'monitor your current usage, head to: https://ai.dev/rate-limit. \n* Quota exceeded for metric: '
                    'generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 20, model: '
                    f'{MODEL}\nPlease retry in {wait}s.'),
        'status': 'RESOURCE_EXHAUSTED',
        'details': [
            {'@type': 'type.googleapis.com/google.rpc.QuotaFailure', 'violations': [{
                'quotaMetric': 'generativelanguage.googleapis.com/generate_content_free_tier_requests',
                'quotaId': quota_id, 'quotaDimensions': {'location': 'global', 'model': MODEL}, 'quotaValue': '20'}]},
            {'@type': 'type.googleapis.com/google.rpc.RetryInfo', 'retryDelay': f'{int(float(wait))}s'},
        ]}})


def last_user_text(body):
    """The last text the person sent (a failed turn's message and the next one share a content)."""
    for content in reversed(body.get('contents', [])):
        texts = [part['text'] for part in content.get('parts', []) if 'text' in part and part['text'].strip()]
        if content.get('role') == 'user' and texts:
            return next((text for text in reversed(texts) if any(word.isupper() and '_' in word or word == 'GLOSSARY'
                                                                  for word in text.split())), texts[-1])
    return ''


def responses(body):
    """This turn's function responses: those after the last user text."""
    contents = body.get('contents', [])
    start = max((index for index, content in enumerate(contents) if content.get('role') == 'user'
                 and any('text' in part for part in content.get('parts', []))), default=0)
    return [part['functionResponse'] for content in contents[start:]
            for part in content.get('parts', []) if 'functionResponse' in part]


class Gemini(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body, content_type='application/json'):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.end_headers()
        self.wfile.write(body.encode())

    def do_GET(self):
        self.reply(200, json.dumps({'models': [{
            'name': f'models/{MODEL}', 'displayName': 'Gemini 3.8 Flash', 'thinking': True,
            'inputTokenLimit': 1048576, 'outputTokenLimit': 65536,
            'supportedGenerationMethods': ['generateContent'],
        }]}))

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        REQUESTS.append({'path': self.path, 'body': body})
        for tool in body.get('tools', []):
            for declaration in tool.get('functionDeclarations', []):
                refused = refused_shape(declaration.get('parametersJsonSchema', {}))
                if refused:
                    self.reply(400, json.dumps({'error': {'code': 400, 'message': refused, 'status': 'INVALID_ARGUMENT'}}))
                    return
        asked = last_user_text(body)
        if 'DAILY_QUOTA' in asked:
            self.reply(429, quota_refusal('GenerateRequestsPerDayPerProjectPerModel-FreeTier'))
            return
        if 'MINUTE_QUOTA' in asked and not MINUTE_REFUSED:
            MINUTE_REFUSED.append(True)
            self.reply(429, quota_refusal('GenerateRequestsPerMinutePerProjectPerModel-FreeTier', '1.2'))
            return
        answered = responses(body)
        if 'GLOSSARY' in asked and not answered:
            parts = [{'functionCall': {'name': 'mcp__glossary__lookup', 'args': {'word': 'zorp', 'language': 'en'}},
                      'thoughtSignature': 'fixture-signature'}]
        elif answered:
            parts = [{'text': 'ANSWERED ' + json.dumps(answered[-1]['response'])}]
        else:
            parts = [{'text': 'ANSWERED ' + asked.split()[0]}]
        event = {'responseId': 'fixture', 'candidates': [{'content': {'role': 'model', 'parts': parts},
                                                           'finishReason': 'STOP'}],
                 'usageMetadata': {'promptTokenCount': 40, 'candidatesTokenCount': 5}}
        self.reply(200, 'data: ' + json.dumps(event) + '\n\n', 'text/event-stream')


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Gemini)
threading.Thread(target=service.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix='haarness-gemini-refusals-',
                                 dir=os.environ.get('HAARNESS_E2E_TMP')) as temporary:
    root = Path(temporary).resolve()
    home, workspace = root / 'home', root / 'workspace'
    home.mkdir()
    (workspace / '.haarness').mkdir(parents=True)
    server = root / 'glossary.py'
    server.write_text(MCP_SERVER)
    (workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="gemini"\nmodel="{MODEL}"\napproval_mode="review"\n'
        '[permissions]\nread="allow"\n"mcp__glossary__*"="allow"\n'
        '[lazy_tools]\nmode="off"\n[lsp]\nenabled=false\n'
        f'[mcp.glossary]\ntransport="stdio"\ncommand={json.dumps(sys.executable)}\n'
        f'args=["-I", {json.dumps(str(server))}]\nenabled=true\n')
    run(['git', 'init', '-q', str(workspace)])
    env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
           'HAARNESS_DATA_DIR': str(root / 'data'), 'USER': os.environ.get('USER', 'fixture'),
           'LANG': 'en_US.UTF-8', 'GEMINI_API_KEY': 'fixture-key',
           'GEMINI_BASE_URL': f'http://127.0.0.1:{service.server_port}'}
    args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
    connection = None
    try:
        connection = json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))

        def api(path, body=None):
            request = urllib.request.Request(connection['url'] + '/v1' + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return json.loads(raw) if raw else None

        until(lambda: any(p['id'] == 'gemini' and any(m['id'] == MODEL for m in p['models'])
                          for p in api('/providers')), 'mock Gemini catalog was not discovered')
        thread = api('/threads', {'title': 'Gemini refusals', 'provider': 'gemini', 'model': MODEL})['id']

        def items():
            return api(f'/threads/{thread}/items')

        def turn(text, timeout=60):
            seen, offset = len(items()), len(REQUESTS)
            started = time.monotonic()
            api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})
            until(lambda: len(REQUESTS) > offset, f'{text} reached no provider')
            until(lambda: not api('/turns/active'), f'{text} stayed active', timeout)
            return REQUESTS[offset:], items()[seen:], time.monotonic() - started

        # 1. The MCP tools reach Gemini in a shape it takes; the model calls one and reads its result.
        until(lambda: any(item['name'] == 'glossary' and item['status'] == 'connected' for item in api('/mcp')),
              'the MCP fixture did not connect')
        sent, new, _ = turn('GLOSSARY what does zorp mean?')
        declarations = {d['name']: d['parametersJsonSchema'] for d in sent[0]['body']['tools'][0]['functionDeclarations']}
        assert {'mcp__glossary__lookup', 'mcp__glossary__between', 'mcp__glossary__pair',
                'mcp__glossary__outline'} <= set(declarations), sorted(declarations)
        lookup = declarations['mcp__glossary__lookup']
        assert lookup['required'] == ['word', 'language'] and lookup['properties']['language'] == {}, lookup
        assert all(refused_shape(schema) is None for schema in declarations.values()), declarations
        errors = [i['data']['message'] for i in new if i['data']['type'] == 'error']
        assert not errors, errors
        assert any(i['data']['type'] == 'tool_result' and 'GLOSSARY_PROOF' in i['data']['output']
                   for i in new), new
        assert any(i['data']['type'] == 'assistant_message' and 'GLOSSARY_PROOF' in i['data']['text']
                   for i in new), new

        # 2. A day's quota used up: one request, no waits, words that say when it comes back.
        sent, new, took = turn('DAILY_QUOTA please')
        assert len(sent) == 1, f'a day quota was retried: {len(sent)} requests'
        assert took < 10, f'a day quota was waited on for {took:.1f}s'
        errors = [i['data']['message'] for i in new if i['data']['type'] == 'error']
        assert errors and errors[-1].startswith(
            "This API key has used up today's free allowance for Gemini 3.8 Flash on Google Gemini, "
            'which comes back at midnight Pacific time, in about '), errors
        assert 'Wait a minute' not in errors[-1], errors

        # 3. A minute's quota with the same kind of wait is sat out, and the turn answers.
        sent, new, _ = turn('MINUTE_QUOTA please')
        assert len(sent) == 2, (len(sent), [i['data'] for i in new])
        assert not any(i['data']['type'] == 'error' for i in new), new
        assert any(i['data']['type'] == 'assistant_message' and i['data']['text'] == 'ANSWERED MINUTE_QUOTA'
                   for i in new), new

        (EVIDENCE / 'requests.json').write_text(json.dumps(REQUESTS, indent=2))
        (EVIDENCE / 'items.json').write_text(json.dumps(items(), indent=2))
        print('PASS: MCP tools with schemas Gemini refuses reach it in a shape it takes and run; a day quota fails '
              'at once and says when it returns; a minute quota is waited out; evidence:', EVIDENCE)
    finally:
        if connection:
            subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
        service.shutdown()
