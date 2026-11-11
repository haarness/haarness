#!/usr/bin/env python3
"""Real CLI and server against an Ollama stand-in that makes the tool calls small local models make.

Each case is one `haarness run` whose stand-in model sends calls seen from Qwen2.5 Coder, Llama 3.1/3.2
and gpt-oss on Ollama, then answers once their results come back. Checked:

- A chat with no MCP server and no browser is not offered `execute` (JavaScript that can call only those
  tools); a call to it anyway says it isn't available and to run commands with bash.
- `read` of a folder shows what the folder holds, folders ending in `/`, instead of failing.
- `list` of a file says it is a file and that read shows it, not the system's error.
- A path written from the project's folder as if it were the disk's root (`/tests/test_pricing.py`) reads
  and edits the project's file; one that names a real place (`/tmp/...`) stays as written.
- Fields `read` and `list` do not take (`line_start`, `line_end`, `depth`) are left out with a note.
- multiedit's `edits` written as the JSON text of the array apply, also with the quoted code's line breaks in it.
- A glob that finds nothing says so, and that `**/` looks in folders below.
- A grep pattern that is no regular expression (`discount(`) is searched for as written.
- A shell call's `timeout` under a second (`"120"`, meant as seconds) is read as seconds, and `"False"` as false.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-ollama-tool-calls.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import http.server
import json
import os
import pathlib
import socketserver
import subprocess
import tempfile
import threading

import e2e_scratch
from e2e_env import clean_env

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-ollama-calls-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root / 'shop'
(workspace / 'src/shop').mkdir(parents=True)
(workspace / 'tests').mkdir()
(workspace / 'README.md').write_text('# shop\n\nOrder pricing for the checkout page.\n')
PRICING = 'BULK_THRESHOLD = 100.0\nBULK_DISCOUNT = 0.10\n\n\ndef discount(subtotal):\n    if subtotal > BULK_THRESHOLD:\n        return round(subtotal * BULK_DISCOUNT, 2)\n    return 0.0\n'
(workspace / 'src/shop/pricing.py').write_text(PRICING)
(workspace / 'src/shop/cart.py').write_text('class Cart:\n    def __init__(self):\n        self.items = []\n')
(workspace / 'tests/test_pricing.py').write_text('import unittest\n\n\nclass PricingTest(unittest.TestCase):\n    pass\n')
MODEL = 'fixture-coder:7b'

# Each case: the calls the model makes, one round each, then its answer. Paths in braces are filled in.
CASES = {
    'execute': [('execute', {'code': 'ls -la'})],
    'folder': [('read', {'path': '{workspace}'}), ('read', {'path': 'src'})],
    'list-file': [('list', {'path': 'src/shop/cart.py'})],
    'rooted': [
        ('read', {'path': '/src/shop/cart.py'}),
        ('edit', {'path': '/tests/test_pricing.py', 'old': '    pass\n', 'new': '    def test_clear(self):\n        pass\n'}),
        ('write', {'path': '/src/shop/notes.txt', 'content': 'written\n'}),
        ('read', {'path': '{tmp}/haarness-no-such-file.txt'}),
    ],
    'fields': [
        ('read', {'path': 'README.md', 'line_start': 1, 'line_end': 400}),
        ('list', {'path': 'src/shop', 'depth': 1}),
    ],
    'edits-text': [('multiedit', {'path': 'src/shop/pricing.py', 'edits': json.dumps([
        {'new': 'BULK_MINIMUM', 'old': 'BULK_THRESHOLD', 'replaceAll': True},
        {'new': 'BULK_RATE', 'old': 'BULK_DISCOUNT', 'replaceAll': 'true'},
    ])})],
    # Qwen3 Coder 30B: the JSON text keeps the quoted code's line breaks as they are.
    'edits-lines': [('multiedit', {'path': 'src/shop/cart.py', 'edits': '[{"old": "    def __init__(self):\n        self.items = []\n", "new": "    def __init__(self):\n        self.items = []\n\n    def clear(self):\n        self.items = []\n"}]'})],
    'glob-none': [('glob', {'path': 'src', 'pattern': '*.py'})],
    'grep-text': [('grep', {'path': 'src', 'pattern': 'discount('})],
    'shell-seconds': [('bash', {'command': 'sleep 0.5 && echo ran', 'timeout': '120', 'run_in_background': 'False'})],
    # Windows reads `\src\x` from the drive's root: `C:\src\x`.
    'rooted-backslash': [('read', {'path': '\\src\\shop\\cart.py'})],
}
requests = []


def filled(value):
    if isinstance(value, str):
        return value.replace('{workspace}', str(workspace)).replace('{tmp}', tempfile.gettempdir())
    if isinstance(value, dict):
        return {key: filled(item) for key, item in value.items()}
    return value


class Ollama(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body, kind='application/json'):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == '/api/tags':
            return self.reply(200, {'models': [{'name': MODEL, 'model': MODEL}]})
        if self.path == '/api/version':
            return self.reply(200, {'version': '0.40.6'})
        self.reply(404, {'error': 'not found'})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length') or 0)) or b'{}')
        if self.path == '/api/show':
            return self.reply(200, {'capabilities': ['completion', 'tools'], 'details': {'parameter_size': '7.6B'},
                                    'model_info': {'general.architecture': 'qwen2', 'qwen2.context_length': 32_768}})
        if self.path != '/api/chat':
            return self.reply(404, {'error': 'not found'})
        messages = body.get('messages', [])
        prompts = [m.get('content', '') for m in messages if m.get('role') == 'user']
        case = next((name for name in CASES if any(f'case:{name}.' in prompt for prompt in prompts)), None)
        if case is None:
            # A title or other side request: a short answer.
            lines = [{'message': {'role': 'assistant', 'content': 'Fixture'}, 'done': False}]
        else:
            requests.append({'case': case, 'tools': [tool['function']['name'] for tool in body.get('tools') or []],
                             'results': [m.get('content', '') for m in messages if m.get('role') == 'tool']})
            done = len(requests[-1]['results'])
            script = CASES[case]
            if done < len(script):
                name, arguments = script[done]
                call = {'function': {'name': name, 'arguments': filled(arguments)}}
                lines = [{'message': {'role': 'assistant', 'content': '', 'tool_calls': [call]}, 'done': False}]
            else:
                lines = [{'message': {'role': 'assistant', 'content': f'Done with {case}.'}, 'done': False}]
        lines.append({'message': {'role': 'assistant', 'content': ''}, 'done': True, 'done_reason': 'stop',
                      'prompt_eval_count': 100, 'eval_count': 10})
        self.reply(200, ''.join(json.dumps(line) + '\n' for line in lines).encode(), 'application/x-ndjson')


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


ollama = Server(('127.0.0.1', 0), Ollama)
threading.Thread(target=ollama.serve_forever, daemon=True).start()
env = clean_env('OLLAMA_')
env.update(HAARNESS_DATA_DIR=str(root / 'state'), HAARNESS_HOME=str(root / 'home'), HAARNESS_NO_CONNECT_PROMPT='1',
           OLLAMA_HOST=f'127.0.0.1:{ollama.server_address[1]}')
env.pop('HAARNESS_TEST_PROVIDER', None)
(workspace / '.haarness').mkdir()
(workspace / '.haarness/config.toml').write_text('[providers.lmstudio]\nenabled = false\n')


def run(case):
    result = subprocess.run([str(binary), '--workspace', str(workspace), '--trust-project', 'run', '--yolo', '-m',
                             f'ollama/{MODEL}', f'Please do case:{case}.'], env=env, cwd=workspace,
                            capture_output=True, text=True, timeout=180, stdin=subprocess.DEVNULL)
    out = (result.stdout + result.stderr).strip()
    assert result.returncode == 0, out
    assert f'Done with {case}.' in out, out
    rounds = [request for request in requests if request['case'] == case]
    return rounds, rounds[-1]['results']


def check_execute():
    rounds, results = run('execute')
    assert all('execute' not in request['tools'] for request in rounds), rounds[0]['tools']
    assert 'bash' in rounds[0]['tools'] and 'read' in rounds[0]['tools'], rounds[0]['tools']
    assert "this chat has none, so it isn't available. To run a command, call bash with it." in results[0], results
    return 'execute is not offered without MCP or browser tools, and a call says to use bash'


def check_folder():
    _, results = run('folder')
    assert 'is a folder. It holds (folders end in /):' in results[0], results
    for name in ('README.md', 'src/', 'tests/'):
        assert f'\n{name}' in results[0], results[0]
    assert results[1].endswith('is a folder. It holds (folders end in /):\nshop/'), results[1]
    return 'read of a folder shows what it holds'


def check_list_file():
    _, results = run('list-file')
    assert 'is a file, not a folder; read shows what it holds' in results[0], results
    return 'list of a file says so in plain words'


def check_rooted():
    _, results = run('rooted')
    assert 'self.items = []' in results[0], results
    assert 'does not exist' not in results[1] and 'updated' in results[1], results
    assert 'def test_clear' in (workspace / 'tests/test_pricing.py').read_text()
    assert (workspace / 'src/shop/notes.txt').read_text() == 'written\n', results[2]
    assert 'does not exist' in results[3], results[3]
    return 'a path written from the project as the disk root reaches the project file; a real place stays'


def check_fields():
    _, results = run('fields')
    assert results[0].startswith('# shop'), results
    assert '(Not used: line_start and line_end. read takes path, offset and limit.)' in results[0], results[0]
    assert 'cart.py' in results[1] and '(Not used: depth. list takes path.)' in results[1], results[1]
    return 'fields read and list do not take are left out with a note'


def check_edits_text():
    _, results = run('edits-text')
    assert 'applied 2 edits' in results[0], results
    expected = PRICING.replace('BULK_THRESHOLD', 'BULK_MINIMUM').replace('BULK_DISCOUNT', 'BULK_RATE')
    assert (workspace / 'src/shop/pricing.py').read_text() == expected
    return "multiedit's edits written as JSON text apply"


def check_edits_lines():
    _, results = run('edits-lines')
    assert 'applied 1 edits' in results[0], results
    assert 'def clear(self):' in (workspace / 'src/shop/cart.py').read_text()
    return "multiedit's edits written as JSON text with the code's line breaks as they are apply"


def check_glob_none():
    _, results = run('glob-none')
    assert results[0] == 'No files found. * matches inside one folder only; **/*.py also looks in every folder below it', results
    return 'a glob that finds nothing says so and how to look deeper'


def check_grep_text():
    _, results = run('grep-text')
    assert results[0].startswith('src/shop/pricing.py:5:def discount(subtotal):'), results
    assert results[0].endswith('(discount( is not a regular expression, so it was searched for as written.)'), results
    return 'a grep pattern that is no regular expression is searched for as written'


def check_shell_seconds():
    _, results = run('shell-seconds')
    assert 'ran' in results[0] and 'timed out' not in results[0], results
    assert results[0].endswith('(timeout 120 was read as 120 seconds: it is in milliseconds.)'), results
    return 'a shell timeout under a second is read as seconds, and Python-style False as false'


def check_rooted_backslash():
    _, results = run('rooted-backslash')
    assert 'self.items = []' in results[0], results
    return 'on Windows, a path from the project as the drive root, in backslashes, reads the project file'


failed = []
try:
    for check in (check_execute, check_folder, check_list_file, check_rooted, check_fields, check_edits_text,
                  check_edits_lines, check_glob_none, check_grep_text, check_shell_seconds,
                  *([check_rooted_backslash] if os.name == 'nt' else [])):
        try:
            print(f'PASS {check()}', flush=True)
        except AssertionError as error:
            failed.append(check.__name__)
            print(f'FAIL {check.__name__}: {str(error)[:600]}', flush=True)
    if failed:
        raise SystemExit(f'{len(failed)} failed: {", ".join(failed)}')
finally:
    # `haarness run` started the project's server and left it running.
    e2e_scratch.stop_server(binary, workspace, env)
    ollama.shutdown()
    e2e_scratch.settle(root)
