#!/usr/bin/env python3
"""Real CLI and server against a stand-in for a person's own OpenAI-compatible server (a "custom" provider) whose
model makes the tool calls models trained on other coding agents make, and refuses requests the way Llama 3's chat
template does. Each case is one `haarness run`. Checked:

- Other agents' tool names (`run_command`, `read_file`, `list_directory`) run as the tool here that does that job;
  `str_replace_editor` is answered with the tools that do its commands.
- A shell call written the way other agents' shells take it runs: the command as a list of words, `cmd`,
  `is_background`, a `description`, and optional fields given as null.
- A call that failed twice with the same error is not run a third time, and the model is told what it failed with;
  what it would have done is never done.
- A refused call ends with the fields the tool takes.
- A tool left out of the requests to keep them small (`schedule` under `lazy_tools.mode = "always"`), called by its
  name, runs.
- A command that runs out of a timeout that reads as seconds (1200) is told the unit and the timeout to send.
- A server whose chat template takes one tool call per assistant message (Llama 3's: "This model only supports single
  tool-calls at once!") gets them one by one once the model made two calls at once, and the turn goes on.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-custom-tool-calls.py
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
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-custom-calls-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root / 'shop'
(workspace / 'src').mkdir(parents=True)
(workspace / 'README.md').write_text('# shop\n')
(workspace / 'src/notes.txt').write_text('bulk discount above 100\n')
MODEL = 'fixture-coder'
TEMPLATE_REFUSAL = 'This model only supports single tool-calls at once!'

# Each round is a list of calls the stand-in model makes at once.
CASES = {
    'names': [
        [('run_command', {'command': 'echo by-name'})],
        [('read_file', {'file_path': 'src/notes.txt'})],
        [('list_directory', {'path': 'src'})],
        [('str_replace_editor', {'command': 'view', 'path': 'README.md'})],
    ],
    'shapes': [
        [('bash', {'command': ['echo', 'two words'], 'description': 'Say it'})],
        [('bash', {'cmd': 'echo cmd-field', 'is_background': False})],
        [('bash', {'command': 'echo nulls', 'timeout': None, 'workdir': None})],
    ],
    'repeated': [[('bash', {'command': 'touch made.txt', 'bogus': True})]] * 4,
    'deferred': [[('schedule', {'action': 'list'})]],
    'timeout': [[('bash', {'command': 'sleep 5', 'timeout': 1200})]],
    'parallel': [[('read', {'path': 'README.md'}), ('read', {'path': 'src/notes.txt'})]],
}
requests = []


class Gateway(http.server.BaseHTTPRequestHandler):
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
        if self.path.endswith('/models'):
            return self.reply(200, {'object': 'list', 'data': [{'id': MODEL, 'object': 'model'}]})
        self.reply(404, {'error': {'message': 'not found'}})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length') or 0)) or b'{}')
        messages = body.get('messages', [])
        prompts = [str(m.get('content', '')) for m in messages if m.get('role') == 'user']
        case = next((name for name in CASES if any(f'case:{name}.' in prompt for prompt in prompts)), None)
        if case is None:
            return self.stream({'role': 'assistant', 'content': 'Fixture'}, 'stop')
        several = any(len(m.get('tool_calls') or []) > 1 for m in messages if m.get('role') == 'assistant')
        requests.append({'case': case, 'tools': [tool['function']['name'] for tool in body.get('tools') or []],
                         'results': [m.get('content', '') for m in messages if m.get('role') == 'tool'],
                         'several': several, 'parallel_tool_calls': body.get('parallel_tool_calls')})
        if case == 'parallel' and several:
            return self.reply(400, {'object': 'error', 'message': TEMPLATE_REFUSAL, 'type': 'BadRequestError', 'code': 400})
        rounds_done = sum(1 for m in messages if m.get('role') == 'assistant' and m.get('tool_calls'))
        if case == 'parallel':
            # Split, each call is its own message: the one round of two calls is done when both results are back.
            rounds_done = 1 if requests[-1]['results'] else 0
        script = CASES[case]
        if rounds_done < len(script):
            calls = [{'index': index, 'id': f'{case}-{rounds_done}-{index}', 'type': 'function',
                      'function': {'name': name, 'arguments': json.dumps(arguments)}}
                     for index, (name, arguments) in enumerate(script[rounds_done])]
            return self.stream({'role': 'assistant', 'tool_calls': calls}, 'tool_calls')
        return self.stream({'role': 'assistant', 'content': f'Done with {case}.'}, 'stop')

    def stream(self, delta, finish):
        chunks = [{'choices': [{'index': 0, 'delta': delta}]},
                  {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]},
                  {'choices': [], 'usage': {'prompt_tokens': 100, 'completion_tokens': 10}}]
        data = ''.join(f'data: {json.dumps(chunk)}\n\n' for chunk in chunks) + 'data: [DONE]\n\n'
        self.reply(200, data.encode(), 'text/event-stream')


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


gateway = Server(('127.0.0.1', 0), Gateway)
threading.Thread(target=gateway.serve_forever, daemon=True).start()
env = clean_env()
env.update(HAARNESS_DATA_DIR=str(root / 'state'), HAARNESS_HOME=str(root / 'home'), HAARNESS_NO_CONNECT_PROMPT='1')
env.pop('HAARNESS_TEST_PROVIDER', None)
(workspace / '.haarness').mkdir()
(workspace / '.haarness/config.toml').write_text(
    '[providers.lmstudio]\nenabled = false\n[providers.ollama]\nenabled = false\n'
    '[lazy_tools]\nmode = "always"\n'
    '[providers.mygateway]\nprotocol = "openai-chat-completions"\nname = "My gateway"\n'
    f'base_url = "http://127.0.0.1:{gateway.server_address[1]}/v1"\nmodels = ["{MODEL}"]\n'
    'requires_auth = false\ntools = true\ncontext_window = 131072\n')
subprocess.run(['git', 'init', '-q', str(workspace)], check=True)


def run(case):
    result = subprocess.run([str(binary), '--workspace', str(workspace), '--trust-project', 'run', '--yolo', '-m',
                             f'mygateway/{MODEL}', f'Please do case:{case}.'], env=env, cwd=workspace,
                            capture_output=True, text=True, timeout=180, stdin=subprocess.DEVNULL)
    out = (result.stdout + result.stderr).strip()
    assert result.returncode == 0, out
    assert f'Done with {case}.' in out, out
    rounds = [request for request in requests if request['case'] == case]
    return rounds, rounds[-1]['results']


def check_names():
    _, results = run('names')
    assert results[0].startswith('exit: 0\nby-name'), results
    assert 'bulk discount above 100' in results[1], results
    assert 'notes.txt' in results[2], results
    assert 'read views a file, write creates one, and edit replaces text in one' in results[3], results
    return "other agents' tool names run as this one's, and str_replace_editor is told which tools do its commands"


def check_shapes():
    _, results = run('shapes')
    assert results == ['exit: 0\ntwo words\n', 'exit: 0\ncmd-field\n', 'exit: 0\nnulls\n'], results
    return 'a command as words, cmd, is_background, a description and null optional fields run'


def check_repeated():
    _, results = run('repeated')
    for result in results[:2]:
        assert 'unexpected fields: bogus' in result, results
        assert 'bash takes command (string, required), timeout (integer), workdir (string) and run_in_background (boolean).' in result, results
    for result in results[2:]:
        assert 'Not run: this exact call already failed 2 times in this turn with the same error' in result, results
        assert 'unexpected fields: bogus' in result, results
    assert not (workspace / 'made.txt').exists()
    return 'a call that failed the same way twice is not run again, and a refusal names the fields the tool takes'


def check_deferred():
    rounds, results = run('deferred')
    assert 'schedule' not in rounds[0]['tools'] and 'load_tool' in rounds[0]['tools'], rounds[0]['tools']
    assert results == ['[]'], results
    assert 'schedule' in rounds[-1]['tools'], rounds[-1]['tools']
    return 'a deferred tool called by name is loaded and runs'


def check_timeout():
    _, results = run('timeout')
    assert 'command timed out after 1200 ms' in results[0], results
    assert 'The timeout is in milliseconds: 1200 is 1.2 seconds. To give the command 1200 seconds, send timeout 1200000.' in results[0], results
    return 'a command out of time is told the unit and the timeout to send'


def check_parallel():
    rounds, results = run('parallel')
    refused = [request for request in rounds if request['several']]
    split = [request for request in rounds if not request['several'] and request['results']]
    assert len(refused) == 1 and split and split[-1]['parallel_tool_calls'] is False, rounds
    assert results[0].startswith('1\t# shop') or '# shop' in results[0], results
    assert 'bulk discount above 100' in results[1], results
    return "a chat template that takes one call a message gets the calls one by one, and the turn goes on"


failed = []
try:
    for check in (check_names, check_shapes, check_repeated, check_deferred, check_timeout, check_parallel):
        try:
            print(f'PASS {check()}', flush=True)
        except AssertionError as error:
            failed.append(check.__name__)
            print(f'FAIL {check.__name__}: {str(error)[:900]}', flush=True)
    if failed:
        raise SystemExit(f'{len(failed)} failed: {", ".join(failed)}')
finally:
    gateway.shutdown()
    e2e_scratch.settle(root)
