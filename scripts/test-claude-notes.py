#!/usr/bin/env python3
"""Real CLI proof that the note Claude 5.5 writes before a tool call is reply text, and goes back to Claude once, as
the thinking block it came in.

A mock Anthropic Messages server answers as Claude Opus 5.5 does: a thinking block, a second thinking block whose
signature says it is a note for the person, then a `bash` call; after the call, the answer. Against it, `haarness run`:

1. with `--no-thinking`, prints the note as reply text before the answer, and not the thinking;
2. by default prints the thinking, then the note as reply text of its own, not as the end of the thought;
3. with `--format json`, marks the note `"note": true` and the answer not;
4. sends, in the request after the call, both thinking blocks back once and unchanged in the assistant's call
   message, and the note's words nowhere else;
5. with HAARNESS_BASE_BIN (a build from before notes were told apart), sends that request byte for byte as the base
   build does.

Usage: python3 scripts/test-claude-notes.py   (HAARNESS_BIN picks the binary, HAARNESS_BASE_BIN the build to compare
with, HAARNESS_E2E_ROOT a folder to keep the fixture in, HAARNESS_E2E_EVIDENCE the evidence folder).
"""
import base64
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading

from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
BASE = os.environ.get('HAARNESS_BASE_BIN')
EVIDENCE = Path(os.environ.get('HAARNESS_E2E_EVIDENCE', REPO / 'target/claude-notes/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
MODEL = 'claude-opus-5-5'
THOUGHT = 'Checking why the tests fail.'
NOTE = "I'll run the tests now."
ANSWER = 'All tests pass.'
REQUESTS = []


def delimited(number, body):
    return bytes([(number << 3) | 2, len(body)]) + body


def signature(kind):
    """A signature laid out as Claude's are: field 2 holds a message whose field 1 is a header naming the block's kind
    in its field 8, followed by a sealed part."""
    header = bytes([0x08, 0x13, 0x10, 0x02, 0x18, 0x02]) + delimited(8, kind.encode())
    inner = delimited(1, header) + delimited(3, b'\x5a' * 12)
    return base64.b64encode(bytes([0x08, 0x04]) + delimited(2, inner)).decode()


THINKING_SIGNATURE = signature('thinking')
NOTE_SIGNATURE = signature('narration')


def noted_round():
    events = [{'type': 'message_start', 'message': {'id': 'msg_notes_1', 'usage': {'input_tokens': 40, 'output_tokens': 0}}}]
    for index, (pieces, signed) in enumerate([([THOUGHT], THINKING_SIGNATURE), (["I'll run the ", 'tests now.'], NOTE_SIGNATURE)]):
        events.append({'type': 'content_block_start', 'index': index, 'content_block': {'type': 'thinking', 'thinking': ''}})
        events += [{'type': 'content_block_delta', 'index': index, 'delta': {'type': 'thinking_delta', 'thinking': piece}}
                   for piece in pieces]
        events += [{'type': 'content_block_delta', 'index': index, 'delta': {'type': 'signature_delta', 'signature': signed}},
                   {'type': 'content_block_stop', 'index': index}]
    events += [{'type': 'content_block_start', 'index': 2,
                'content_block': {'type': 'tool_use', 'id': 'toolu_notes_1', 'name': 'bash', 'input': {}}},
               {'type': 'content_block_delta', 'index': 2,
                'delta': {'type': 'input_json_delta', 'partial_json': json.dumps({'command': 'printf ok'})}},
               {'type': 'content_block_stop', 'index': 2},
               {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use', 'stop_sequence': None}, 'usage': {'output_tokens': 30}},
               {'type': 'message_stop'}]
    return events


def answer(text, message):
    return [{'type': 'message_start', 'message': {'id': message, 'usage': {'input_tokens': 60, 'output_tokens': 0}}},
            {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
            {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': text}},
            {'type': 'content_block_stop', 'index': 0},
            {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 4}},
            {'type': 'message_stop'}]


class Anthropic(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'data': [{'id': MODEL, 'display_name': 'Fixture Opus'}], 'has_more': False}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        # A title or another side request offers no tools.
        if not body.get('tools'):
            return self.stream(answer('Fixture title', 'msg_notes_title'))
        REQUESTS.append(body)
        last = body['messages'][-1]['content']
        answered = isinstance(last, list) and any(block.get('type') == 'tool_result' for block in last)
        self.stream(answer(ANSWER, 'msg_notes_2') if answered else noted_round())

    def stream(self, events):
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for event in events:
            self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-claude-notes-')
root = Path(keep or holder.name).resolve()
root.mkdir(parents=True, exist_ok=True)
workspace = root / 'workspace'
(workspace / '.haarness').mkdir(parents=True, exist_ok=True)
(workspace / '.haarness/config.toml').write_text(f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
subprocess.run(['git', 'init', '-q', str(workspace)], check=True)
checks = []


def run(binary, name, *extra):
    """`haarness run` from `binary` in its own data folder: its output and the requests that offered tools."""
    home, data = root / name / 'home', root / name / 'data'
    home.mkdir(parents=True, exist_ok=True)
    env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
           'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
           'ANTHROPIC_API_KEY': 'fixture-key', 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}/v1'}
    first = len(REQUESTS)
    try:
        output = subprocess.run([str(binary), '--workspace', str(workspace), '--trust-project', 'run', '--yolo',
                                 '--model', f'anthropic/{MODEL}', *extra, 'fix the failing tests'],
                                check=True, capture_output=True, text=True, env=env, timeout=180).stdout
    finally:
        subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env,
                       capture_output=True, timeout=60)
    (EVIDENCE / f'{name}.out').write_text(output)
    return output, REQUESTS[first:]


def check(label, condition, detail=''):
    checks.append(label)
    assert condition, f'{label}: {detail}'


def replayed(requests):
    """The request after the call."""
    check('two requests offered tools', len(requests) == 2, [len(requests)])
    return requests[1]


try:
    # 1. The note prints as reply text, before the answer.
    output, _ = run(BINARY, 'replies', '--no-thinking')
    check('the note and the answer print as reply text', output.splitlines() == [NOTE, ANSWER], repr(output))

    # 2. The thought prints as thinking, and the note after it as a reply.
    output, requests = run(BINARY, 'default')
    check('the thought is thinking and the note a reply', output == f'thinking:\n{THOUGHT}\n\n{NOTE}\n{ANSWER}\n',
          repr(output))

    # 3. JSON marks the note.
    marked, _ = run(BINARY, 'json', '--format', 'json')
    records = [json.loads(line) for line in marked.splitlines() if line.strip()]
    replies = [(record['text'], record.get('note')) for record in records if record['type'] == 'assistant']
    check('JSON marks the note and not the answer', replies == [(NOTE, True), (ANSWER, None)], records)

    # 4. Claude gets the note back once, inside its thinking block, beside the call.
    body = replayed(requests)
    (EVIDENCE / 'replayed-request.json').write_text(json.dumps(body, indent=2, sort_keys=True) + '\n')
    call = next(message for message in body['messages'] if message['role'] == 'assistant')
    check('the call message carries both blocks, unchanged, then the call', call['content'] == [
        {'type': 'thinking', 'thinking': THOUGHT, 'signature': THINKING_SIGNATURE},
        {'type': 'thinking', 'thinking': NOTE, 'signature': NOTE_SIGNATURE},
        {'type': 'tool_use', 'id': 'toolu_notes_1', 'name': 'bash', 'input': {'command': 'printf ok'}},
    ], call)
    mentions = sum(json.dumps(message, ensure_ascii=False).count(NOTE) for message in body['messages'])
    check("the note's words go back once", mentions == 1, mentions)

    # 5. The same request as before notes were told apart.
    if BASE:
        output, before = run(Path(BASE).resolve(), 'base')
        check('the base build printed the note as the end of the thought',
              output == f'thinking:\n{THOUGHT}\n\n{NOTE}\n\n{ANSWER}\n', repr(output))
        base_body = replayed(before)
        (EVIDENCE / 'replayed-request-base.json').write_text(json.dumps(base_body, indent=2, sort_keys=True) + '\n')
        check('the request after the call is byte-identical to the base build\'s',
              json.dumps(body, sort_keys=True) == json.dumps(base_body, sort_keys=True),
              'see replayed-request*.json in ' + str(EVIDENCE))
    else:
        print('HAARNESS_BASE_BIN not set: the byte comparison with a base build was skipped')
    print(f'{len(checks)} checks passed:')
    for label in checks:
        print('  ok', label)
finally:
    service.shutdown()
    if holder:
        holder.cleanup()
