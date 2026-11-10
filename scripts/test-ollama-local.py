#!/usr/bin/env python3
"""Real CLI and server against an Ollama stand-in, no network and no model: how Haarness runs Ollama models.

The stand-in answers Ollama's own API (`/api/tags`, `/api/show`, `/api/chat`) on a free port that
`OLLAMA_HOST` names, and like Ollama on a computer with a small GPU it runs a model with 4K tokens
unless the request asks for more. Checked:

- Haarness finds Ollama where `OLLAMA_HOST` says, though it takes most of a second to list its models, and
  lists its model with the 32K window it runs it with.
- A chat sends Ollama's own chat request with that window (`num_ctx`) and `truncate` off, and a call the
  model writes as text (as Qwen2.5 Coder does) runs: the file is read and the model answers from it.
- A model Ollama runs without tools is refused, naming one to use instead; nothing is sent to it.
- A model Ollama hasn't downloaded says to `ollama pull` it.
- A cloud model while Ollama isn't signed in says to `ollama signin`.
- A model too short for the instructions and tools says so, naming one with room.
- A model that answers with nothing says to choose a larger one.
- A new chat on Ollama alone gets a model that can do the work, not the first Ollama lists.

A second stand-in serves gpt-oss as Ollama 0.40.6 does:

- A chat's second message, past the compaction threshold, compacts: asked to summarize, gpt-oss calls a tool it was
  never offered, is asked again, and its summary is used; the new message isn't summarized but kept as it is.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-ollama-local.py
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
import time

import e2e_scratch
from e2e_env import clean_env

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-ollama-local-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'
workspace.mkdir()
(workspace/'calc.py').write_text('def add(a, b):\n    return a - b\n')

# name -> (capabilities, longest context, parameters), in the order Ollama lists them
MODELS = {
    'gemma3:latest': (['completion', 'vision'], 131_072, '4.3B'),
    'gpt-oss:120b-cloud': (['completion', 'tools', 'thinking'], 131_072, '116829156672'),
    'qwen2.5-coder:7b': (['completion', 'tools', 'insert'], 32_768, '7.6B'),
    'smollm2:135m': (['completion', 'tools'], 8_192, '135M'),
    'tinyllama:1.1b': (['completion', 'tools'], 2_048, '1.1B'),
}
chats = []


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
            # A busy computer: Ollama takes most of a second to list its models.
            time.sleep(0.8)
            return self.reply(200, {'models': [{'name': name, 'model': name} for name in MODELS]})
        if self.path == '/v1/models':
            return self.reply(200, {'object': 'list', 'data': [{'id': name, 'object': 'model'} for name in MODELS]})
        if self.path == '/api/version':
            return self.reply(200, {'version': '0.40.6'})
        self.reply(404, {'error': 'not found'})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length') or 0)) or b'{}')
        if self.path == '/api/show':
            name = body.get('model')
            if name not in MODELS:
                return self.reply(404, {'error': f"model '{name}' not found"})
            capabilities, longest, parameters = MODELS[name]
            return self.reply(200, {'capabilities': capabilities, 'details': {'parameter_size': parameters}, 'model_info': {'general.architecture': 'arch', 'arch.context_length': longest}})
        if self.path == '/v1/chat/completions':
            # Ollama's OpenAI-style endpoint: the server's 4K, the front of the prompt cut.
            chats.append({'endpoint': 'openai-style', **body})
            line = {'choices': [{'delta': {'content': "I'm ready to help!"}, 'finish_reason': 'stop'}]}
            return self.reply(200, f'data: {json.dumps(line)}\n\ndata: [DONE]\n\n'.encode(), 'text/event-stream')
        if self.path != '/api/chat':
            return self.reply(404, {'error': 'not found'})
        chats.append(body)
        name = body.get('model')
        if name not in MODELS:
            return self.reply(404, {'error': f"model '{name}' not found"})
        if name.endswith('-cloud'):
            return self.reply(401, {'error': 'Unauthorized'})
        window = min((body.get('options') or {}).get('num_ctx') or 4_096, MODELS[name][1])
        # Roughly Ollama's count: a token for every four characters.
        tokens = len(json.dumps(body.get('messages'))) // 4 + len(json.dumps(body.get('tools') or [])) // 4
        if tokens > window and body.get('truncate') is False:
            return self.reply(400, {'error': f'request ({tokens} tokens) exceeds the available context size ({window} tokens), try increasing it'})
        tool_results = [m for m in body.get('messages', []) if m.get('role') == 'tool']
        if name == 'smollm2:135m':
            # A tiny model lost in the instructions: nothing comes back.
            lines = [{'message': {'role': 'assistant', 'content': '\n'}, 'done': False}]
        elif tool_results:
            text = 'add() subtracts: it returns a - b.' if 'a - b' in tool_results[-1].get('content', '') else 'I could not read it.'
            lines = [{'message': {'role': 'assistant', 'content': text}, 'done': False}]
        else:
            # Qwen2.5 Coder writes its call as text, with no <tool_call> around it.
            lines = [
                {'message': {'role': 'assistant', 'content': '{"name": "read", '}, 'done': False},
                {'message': {'role': 'assistant', 'content': '"arguments": {"path": "calc.py"}}'}, 'done': False},
            ]
        lines.append({'message': {'role': 'assistant', 'content': ''}, 'done': True, 'done_reason': 'stop', 'prompt_eval_count': tokens, 'eval_count': 12})
        self.reply(200, ''.join(json.dumps(line) + '\n' for line in lines).encode(), 'application/x-ndjson')


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


oss_chats = []


class GptOss(Ollama):
    """Ollama with gpt-oss:20b alone, which calls the tools it was trained with when asked to summarize a chat that
    ends on a request."""

    def do_GET(self):
        if self.path == '/api/tags':
            return self.reply(200, {'models': [{'name': 'gpt-oss:20b', 'model': 'gpt-oss:20b'}]})
        if self.path == '/v1/models':
            return self.reply(200, {'object': 'list', 'data': [{'id': 'gpt-oss:20b', 'object': 'model'}]})
        return super().do_GET()

    def stream(self, lines):
        self.reply(200, ''.join(json.dumps(line) + '\n' for line in lines).encode(), 'application/x-ndjson')

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length') or 0)) or b'{}')
        if self.path == '/api/show':
            return self.reply(200, {'capabilities': ['completion', 'tools', 'thinking'], 'details': {'parameter_size': '20.9B'},
                                    'model_info': {'general.architecture': 'gptoss', 'gptoss.context_length': 131_072}})
        if self.path != '/api/chat':
            return self.reply(404, {'error': 'not found'})
        text = json.dumps(body.get('messages'))
        if 'Name this conversation' in text:
            return self.stream([{'message': {'role': 'assistant', 'content': 'Snake game'}, 'done': False},
                                {'message': {'role': 'assistant', 'content': ''}, 'done': True, 'done_reason': 'stop'}])
        oss_chats.append(body)
        done = lambda prompt=6_000: {'message': {'role': 'assistant', 'content': ''}, 'done': True, 'done_reason': 'stop',
                                     'prompt_eval_count': prompt, 'eval_count': 40}
        say = lambda words: {'message': {'role': 'assistant', 'content': words}, 'done': False}
        if not body.get('tools'):
            if 'called a tool instead of giving the summary' in text:
                return self.stream([say('The person had the project looked over; calc.py has an add() bug.'), done()])
            # As gpt-oss did 5 times in 5 with Ollama 0.40.6: a call to its own tool, though none was offered.
            call = {'id': 'call_1', 'function': {'index': 0, 'name': 'repo_browser.open_file', 'arguments': {'path': 'calc.py'}}}
            return self.stream([{'message': {'role': 'assistant', 'content': '', 'thinking': 'Open it.'}, 'done': False},
                                {'message': {'role': 'assistant', 'content': '', 'tool_calls': [call]}, 'done': False}, done()])
        # The first message of the long chat leaves it past its threshold (24K of a 32K window).
        return self.stream([say('I looked over the project.'), done(30_000)])


ollama = Server(('127.0.0.1', 0), Ollama)
threading.Thread(target=ollama.serve_forever, daemon=True).start()
oss = Server(('127.0.0.1', 0), GptOss)
threading.Thread(target=oss.serve_forever, daemon=True).start()
env = clean_env('OLLAMA_')
env.update(HAARNESS_DATA_DIR=str(root/'state'), HAARNESS_HOME=str(root/'home'), HAARNESS_NO_CONNECT_PROMPT='1',
           OLLAMA_HOST=f'127.0.0.1:{ollama.server_address[1]}')
env.pop('HAARNESS_TEST_PROVIDER', None)
# LM Studio, if one runs here, stays out of it.
(workspace/'.haarness').mkdir()
(workspace/'.haarness/config.toml').write_text('[permissions]\nread = "allow"\n[providers.lmstudio]\nenabled = false\n')


def haarness(*args):
    return subprocess.run([str(binary), '--workspace', str(workspace), '--trust-project', *args], env=env,
                          cwd=workspace, capture_output=True, text=True, timeout=180, stdin=subprocess.DEVNULL)


def run(model, prompt='What does add() in calc.py do?'):
    result = haarness('run', '--yolo', '-m', f'ollama/{model}', prompt)
    return result.returncode, (result.stdout + result.stderr).strip()


try:
    listed = haarness('providers', '--verbose')
    assert listed.returncode == 0, listed.stderr
    providers, text, decoder = {}, listed.stdout.strip(), json.JSONDecoder()
    while text:
        provider, end = decoder.raw_decode(text)
        providers[provider['id']] = provider
        text = text[end:].strip()
    assert 'ollama' in providers, f'Ollama at OLLAMA_HOST was not found: {sorted(providers)}'
    coder = next(model for model in providers['ollama']['models'] if model['id'] == 'qwen2.5-coder:7b')
    assert coder['contextWindow'] == 32_768, coder
    assert coder['tools'] and not next(model for model in providers['ollama']['models'] if model['id'] == 'gemma3:latest')['tools']
    # A new chat gets a provider's first model: one that can do the work, not the first Ollama lists.
    order = [model['id'] for model in providers['ollama']['models']]
    assert order[0] == 'qwen2.5-coder:7b', order
    assert order.index('smollm2:135m') < order.index('gpt-oss:120b-cloud') < order.index('gemma3:latest'), order
    print('PASS Ollama is found where OLLAMA_HOST says, with a 32K window and a model that can work first')

    result = haarness('run', '--yolo', '--provider', 'ollama', 'What does add() in calc.py do?')
    code, out = result.returncode, (result.stdout + result.stderr).strip()
    assert code == 0, out
    assert chats[0].get('model') == 'qwen2.5-coder:7b', chats[0].get('model')
    assert 'add() subtracts' in out, out
    first = chats[0]
    assert first.get('endpoint') != 'openai-style', 'sent to the OpenAI-style endpoint, which runs the model with 4K'
    assert first['options']['num_ctx'] == 32_768, first['options']
    assert first['truncate'] is False, first
    assert any(m.get('role') == 'tool' and 'a - b' in m.get('content', '') for m in chats[-1]['messages']), 'the call written as text did not run'
    print('PASS a chat runs with a 32K window, truncation off, and a call written as text runs')

    before = len(chats)
    code, out = run('gemma3:latest')
    assert code != 0, out
    assert "gemma3:latest can't use tools in Ollama, so it can't read, change or run anything in your project." in out, out
    assert 'such as qwen2.5-coder:7b' in out, out
    assert len(chats) == before, 'a model without tools was sent the chat'
    print('PASS a model without tools is refused with one to use')

    code, out = run('qwen2.5-coder:14b')
    assert code != 0, out
    assert "isn't downloaded in Ollama" in out and '`ollama pull qwen2.5-coder:14b`' in out, out
    assert 'no longer offers' not in out, out
    print('PASS a model not downloaded says to pull it')

    code, out = run('gpt-oss:120b-cloud')
    assert code != 0, out
    assert "runs on Ollama's cloud" in out and '`ollama signin`' in out, out
    assert 'API key' not in out, out
    print('PASS a cloud model while Ollama is signed out says to sign in')

    code, out = run('tinyllama:1.1b')
    assert code != 0, out
    assert 'tinyllama:1.1b reads at most 2,048 tokens at once here' in out, out
    assert 'such as qwen2.5-coder:7b' in out, out
    print('PASS a model too short for the instructions and tools says so')

    code, out = run('smollm2:135m')
    assert code != 0, out
    assert 'smollm2:135m sent back an empty reply.' in out and 'such as qwen2.5-coder:7b' in out, out
    print('PASS a model that answers with nothing says to choose a larger one')

    # gpt-oss, on an Ollama of its own and a profile of its own.
    env.update(HAARNESS_DATA_DIR=str(root/'oss-state'), OLLAMA_HOST=f'127.0.0.1:{oss.server_address[1]}')
    before = len(oss_chats)
    code, out = run('gpt-oss:20b', 'Look over the project.')
    assert code == 0, out
    result = haarness('run', '--yolo', '--continue', 'Now read calc.py and explain add().')
    code, out = result.returncode, (result.stdout + result.stderr).strip()
    assert code == 0, out
    summaries = [chat for chat in oss_chats[before:] if not chat.get('tools')]
    assert len(summaries) == 2, [json.dumps(chat['messages'])[-300:] for chat in summaries]
    assert 'called a tool instead of giving the summary' in json.dumps(summaries[1]['messages'])
    assert all('Now read calc.py' not in json.dumps(chat['messages']) for chat in summaries)
    last = oss_chats[-1]
    assert last.get('tools') and 'calc.py has an add() bug' in json.dumps(last['messages']), last['messages']
    assert 'Now read calc.py' in json.dumps(last['messages'])
    print("PASS a summary gpt-oss answers with a tool call is asked for again, and the chat's new message is kept")
finally:
    ollama.shutdown()
    oss.shutdown()
    e2e_scratch.settle(root)
