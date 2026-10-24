#!/usr/bin/env python3
"""A real daemon against a mock Anthropic server that refuses images.

Proves, end to end: images are typed and shrunk on arrival (a large PNG, a JPEG
named .png, an iPhone HEIC on macOS); a provider's 400 "image exceeds 5 MB"
leaves that image out, durably, and the turn still completes after one retry;
the next turn and a turn after a daemon restart send no image and the same
prompt bytes; the TUI shows the notice. A chat with more screenshots than the
provider takes in one request sends the newest: past Anthropic's 32 MB the
oldest are noted before the request goes, in steps that keep the prompt
prefix between them; a provider that refuses more than 20 images gets the
newest after one refusal, and the TUI says the oldest are left out.
"""
import base64
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
import zlib

import e2e_scratch
import e2e_tmux

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
EVIDENCE = Path(os.environ.get('HAARNESS_IMAGE_EVIDENCE', REPO / 'target/image-recovery/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
REQUESTS = []
REJECT = 'REJECT_IMAGES'
# A chat whose first message holds one of these meets a provider that refuses
# more than 20 images in a request, or more than Anthropic's 32 MB.
CAP_IMAGES = 'CAP_IMAGES'
CAP_BYTES = 'CAP_BYTES'
MOST_BYTES = 32 * 1024 * 1024


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


def png(width, height):
    """A noisy RGB PNG, too detailed to compress well."""
    seed = 2463534242
    rows = bytearray()
    for _ in range(height):
        rows.append(0)
        for _ in range(width):
            seed ^= (seed << 13) & 0xffffffff
            seed ^= seed >> 17
            seed ^= (seed << 5) & 0xffffffff
            rows += seed.to_bytes(4, 'little')[:3]

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    header = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(bytes(rows), 6)) + chunk(b'IEND', b'')


def clean(value):
    """Cache breakpoints move with the conversation; the content must not."""
    if isinstance(value, list):
        return [clean(item) for item in value]
    if isinstance(value, dict):
        return {key: clean(item) for key, item in value.items() if key != 'cache_control'}
    return value


def jpeg_size(data):
    offset = 2
    while offset < len(data):
        marker, length = data[offset + 1], struct.unpack('>H', data[offset + 2:offset + 4])[0]
        if 0xc0 <= marker <= 0xc3:
            height, width = struct.unpack('>HH', data[offset + 5:offset + 9])
            return width, height
        offset += 2 + length
    raise AssertionError('no JPEG frame header')


def random_png(width, height):
    """A PNG of random pixels, which no encoder can shrink."""
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    header = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)
    rows = b''.join(b'\0' + os.urandom(width * 3) for _ in range(height))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(rows, 1)) + chunk(b'IEND', b'')


def image_data(body):
    """The base64 of every image block in order, nested tool results included."""
    found = []
    for message in body.get('messages', []):
        content = message.get('content')
        for block in content if isinstance(content, list) else []:
            inner = block.get('content') if block.get('type') == 'tool_result' else None
            for item in [block, *(inner if isinstance(inner, list) else [])]:
                if item.get('type') == 'image':
                    found.append(item['source']['data'])
    return found


def first_text(body):
    content = body['messages'][0]['content']
    return content if isinstance(content, str) else ' '.join(block.get('text', '') for block in content)


def image_blocks(body):
    """(path, media type) of every image block, nested tool results included."""
    found = []
    for index, message in enumerate(body.get('messages', [])):
        content = message.get('content')
        if not isinstance(content, list):
            continue
        for block_index, block in enumerate(content):
            if block.get('type') == 'image':
                found.append((f'messages.{index}.content.{block_index}', block['source']['media_type']))
            inner = block.get('content') if block.get('type') == 'tool_result' else None
            for nested_index, nested in enumerate(inner if isinstance(inner, list) else []):
                if nested.get('type') == 'image':
                    found.append((f'messages.{index}.content.{block_index}.content.{nested_index}', nested['source']['media_type']))
    return found


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'data': [{'id': 'claude-sonnet-4-6', 'type': 'model'}], 'has_more': False}).encode())

    def refuse(self, status, kind, message):
        data = json.dumps({'type': 'error', 'error': {'type': kind, 'message': message}}).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers['Content-Length'])
        body = json.loads(self.rfile.read(length))
        body['bytes'] = length
        REQUESTS.append(body)
        images = image_blocks(body)
        opening = first_text(body) if body.get('messages') else ''
        if CAP_IMAGES in opening and len(images) > 20:
            # No image named: the request as a whole has too many.
            return self.refuse(400, 'invalid_request_error', f'Too many images in the request: {len(images)} > 20')
        if CAP_BYTES in opening and length > MOST_BYTES:
            return self.refuse(413, 'request_too_large', 'Request exceeds the maximum size')
        if images and REJECT in json.dumps(body.get('messages', [])):
            # Anthropic's wording, naming the first image by its place.
            error = {'type': 'error', 'error': {'type': 'invalid_request_error',
                     'message': f'{images[0][0]}.image.source.base64: image exceeds 5 MB maximum: 7340032 bytes > 5242880 bytes'}}
            data = json.dumps(error).encode()
            self.send_response(400)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        events = [
            {'type': 'message_start', 'message': {'usage': {'input_tokens': 21, 'output_tokens': 0}}},
            {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
            {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': f'ANSWER_WITH_{len(images)}_IMAGES'}},
            {'type': 'content_block_stop', 'index': 0},
            {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 5}},
            {'type': 'message_stop'},
        ]
        for event in events:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=service.serve_forever, daemon=True).start()
scratch = Path(os.environ.get('HAARNESS_IMAGE_SCRATCH', tempfile.gettempdir()))
scratch.mkdir(parents=True, exist_ok=True)
root = Path(tempfile.mkdtemp(prefix='haarness-image-recovery-', dir=scratch)).resolve()
home, workspace = root / 'home', root / 'workspace'
home.mkdir()
(workspace / '.haarness').mkdir(parents=True)
(workspace / '.haarness/config.toml').write_text('[defaults]\nprovider="anthropic"\nmodel="claude-sonnet-4-6"\n[lsp]\nenabled=false\n')
run(['git', 'init', '-q', str(workspace)])
env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(root / 'data'), 'USER': os.environ.get('USER', 'fixture'),
       'LANG': 'en_US.UTF-8', 'LC_ALL': 'en_US.UTF-8', 'SSH_TTY': 'fixture',
       'ANTHROPIC_API_KEY': 'fixture-key', 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}'}
args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
session = 'haarness-images-' + uuid.uuid4().hex[:8]
socket = 'haarness-images'
connection = None
results = []


def connect():
    return json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))


def api(path, body=None, method=None):
    request = urllib.request.Request(connection['url'] + '/v1' + path,
                                     data=None if body is None else json.dumps(body).encode(), method=method,
                                     headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def turn(thread, text, attachments=()):
    offset = len(REQUESTS)
    started = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text, 'attachments': list(attachments)})
    until(lambda: not api('/turns/active'), f'turn {text!r} stayed active', timeout=120)
    turn_id = started.get('id') or started.get('turn', {}).get('id')
    state = api(f'/turns/{turn_id}') if turn_id else started
    return state, REQUESTS[offset:]


try:
    connection = connect()

    # 1. A 2600×1400 noisy PNG (about 10 MB): shrunk on arrival, then refused
    # anyway by the provider, left out, and the turn completes.
    big = workspace / 'retina.png'
    big.write_bytes(png(2600, 1400))
    thread = api('/threads', {'title': 'Image recovery', 'provider': 'anthropic', 'model': 'claude-sonnet-4-6'})['id']
    state, sent = turn(thread, f'{REJECT}: what is in this screenshot?', [str(big)])
    assert state['status'] == 'completed', state
    assert len(sent) == 2, f'expected refusal then one retry, saw {len(sent)} requests'
    # Noise does not fit 3.75 MB as a PNG even at 2000 pixels, so it became a JPEG.
    assert image_blocks(sent[0]) == [('messages.0.content.1', 'image/jpeg')], image_blocks(sent[0])
    assert image_blocks(sent[1]) == [], 'the retry still carried the image'
    items = api(f'/threads/{thread}/items')
    stored = next(item['data']['attachments'][0] for item in items if item['data']['type'] == 'user_message')
    assert stored.startswith('data:image/jpeg;name=retina.png;base64,'), stored[:60]
    encoded = base64.b64decode(stored.split(',', 1)[1])
    assert len(encoded) <= 3_750_000 and big.stat().st_size > 3_750_000, (len(encoded), big.stat().st_size)
    assert max(jpeg_size(encoded)) <= 2000, jpeg_size(encoded)
    omitted = [item['data'] for item in items if item['data']['type'] == 'media_omitted']
    assert len(omitted) == 1 and omitted[0]['names'] == ['retina.png'] and not omitted[0].get('guessed'), omitted
    assert 'image exceeds 5 MB maximum' in omitted[0]['reason'], omitted
    assert not any(item['data']['type'] == 'error' for item in items), 'turn reported an error'
    note = json.dumps(sent[1]['messages'][0])
    assert '[Image retina.png omitted' in note, note
    results.append({'check': 'refused image left out, one retry, turn completes',
                    'stored': stored[:30] + '…', 'storedBytes': len(encoded), 'omitted': omitted[0]})
    print('PASS turn 1: refused, left out, retried once, completed', flush=True)

    # 2. The next turn sends no image, so it is not refused.
    state, sent = turn(thread, 'And what should I fix first?')
    assert state['status'] == 'completed' and len(sent) == 1 and image_blocks(sent[0]) == [], (state, len(sent))
    before_restart = json.dumps(clean(sent[0]['messages']), sort_keys=True)
    print('PASS turn 2: no image sent', flush=True)

    # 3. After a daemon restart the omission still holds, byte for byte.
    run(args + ['serve', '--stop'], env=env, timeout=60)
    connection = connect()
    state, sent = turn(thread, 'Still with me after the restart?')
    assert state['status'] == 'completed' and len(sent) == 1 and image_blocks(sent[0]) == [], (state, len(sent))
    replayed = json.dumps(clean(sent[0]['messages'][:3]), sort_keys=True)
    assert replayed == before_restart, 'replayed history changed after restart'
    results.append({'check': 'next turn and after restart: no image, identical prompt prefix'})
    print('PASS turn 3: restart keeps the omission and the prompt bytes', flush=True)
    (EVIDENCE / 'items.json').write_text(json.dumps(api(f'/threads/{thread}/items'), indent=2)[:200_000])

    # 4. Types by bytes: a JPEG saved as .png is sent as image/jpeg; on macOS
    # an iPhone HEIC becomes a JPEG. This provider accepts them.
    if platform.system() == 'Darwin' and shutil.which('sips'):
        small = workspace / 'small.png'
        small.write_bytes(png(320, 200))
        run(['sips', '-s', 'format', 'jpeg', str(small), '--out', str(workspace / 'photo-really-jpeg.png')])
        run(['sips', '-s', 'format', 'heic', str(small), '--out', str(workspace / 'IMG_0001.HEIC')])
        other = api('/threads', {'title': 'Image types', 'provider': 'anthropic', 'model': 'claude-sonnet-4-6'})['id']
        state, sent = turn(other, 'Compare these two',
                           [str(workspace / 'photo-really-jpeg.png'), str(workspace / 'IMG_0001.HEIC')])
        assert state['status'] == 'completed', state
        assert [kind for _, kind in image_blocks(sent[0])] == ['image/jpeg', 'image/jpeg'], image_blocks(sent[0])
        stored = next(item['data']['attachments'] for item in api(f'/threads/{other}/items') if item['data']['type'] == 'user_message')
        assert stored[0] == str(workspace / 'photo-really-jpeg.png'), stored[0]
        assert stored[1].startswith('data:image/jpeg;name=IMG_0001.HEIC;base64,'), stored[1][:60]
        results.append({'check': 'JPEG named .png sent as image/jpeg; HEIC converted to JPEG with sips'})
        print('PASS types: misnamed JPEG and HEIC reach the provider as JPEG', flush=True)

    # 5. Past Anthropic's 32 MB the oldest screenshots are noted before the
    # request goes. Random 1100x1100 PNGs keep their 3.6 MB (4.8 MB of
    # base64): the sixth passes 25 MiB, and the newest three are sent.
    shots = workspace / 'shots'
    shots.mkdir()
    big_thread = api('/threads', {'title': 'Many screenshots', 'provider': 'anthropic', 'model': 'claude-sonnet-4-6'})['id']
    encoded = {}
    kept_counts = []
    previous = None
    for index in range(1, 11):
        shot = shots / f'big-{index:02}.png'
        shot.write_bytes(random_png(1100, 1100))
        encoded[base64.b64encode(shot.read_bytes()).decode()] = shot.name
        text = f'{CAP_BYTES}: screenshot {index}' if index == 1 else f'Screenshot {index}'
        state, sent = turn(big_thread, text, [str(shot)])
        assert state['status'] == 'completed', (index, state)
        assert len(sent) == 1, f'turn {index}: {len(sent)} requests; one was refused'
        assert sent[0]['bytes'] <= MOST_BYTES, (index, sent[0]['bytes'])
        names = [encoded.get(data, '?') for data in image_data(sent[0])]
        assert names[-1] == shot.name, (index, names)
        assert names == sorted(names) and len(set(names)) == len(names), names
        noted = json.dumps(sent[0]['messages']).count("omitted to keep the request within the provider's limits")
        if noted:
            # A screenshot file is named by its path, with how to see it again.
            assert f'[Image {shots}/big-01.png omitted' in json.dumps(sent[0]['messages']), index
            assert 'read the file again' in json.dumps(sent[0]['messages']), index
        assert noted == index - len(names), (index, noted, names)
        kept_counts.append(len(names))
        # Between two cuts each request carries the history of the one before
        # it unchanged: the provider's cache prefix.
        if previous is not None and kept_counts[-1] == kept_counts[-2] + 1:
            prefix = json.dumps(clean(previous['messages']), sort_keys=True)
            again = json.dumps(clean(sent[0]['messages'][:len(previous['messages'])]), sort_keys=True)
            assert prefix == again, f'turn {index}: the history before it changed'
        previous = sent[0]
    assert kept_counts == [1, 2, 3, 4, 5, 3, 4, 5, 3, 4], kept_counts
    assert not any(item['data']['type'] in ('media_omitted', 'error') for item in api(f'/threads/{big_thread}/items'))
    results.append({'check': 'past 25 MiB the oldest are noted, no request refused, the newest sent',
                    'imagesPerRequest': kept_counts})
    print(f'PASS byte budget: images per request {kept_counts}, none refused', flush=True)

    # 6. A provider that refuses more than 20 images, without naming one,
    # gets the newest after one refusal: the oldest half is left out.
    capped = api('/threads', {'title': 'Twenty screenshots', 'provider': 'anthropic', 'model': 'claude-sonnet-4-6'})['id']
    small = {}
    for index in range(1, 23):
        shot = shots / f'shot-{index:02}.png'
        shot.write_bytes(random_png(48, 32))
        small[base64.b64encode(shot.read_bytes()).decode()] = shot.name
        text = f'{CAP_IMAGES}: screenshot {index}' if index == 1 else f'Screenshot {index}'
        state, sent = turn(capped, text, [str(shot)])
        assert state['status'] == 'completed', (index, state)
        names = [small.get(data, '?') for data in image_data(sent[-1])]
        assert names[-1] == shot.name, f'turn {index}: the newest screenshot did not reach the model: {names}'
        if index == 21:
            assert len(sent) == 2, f'expected one refusal, then the retry; saw {len(sent)} requests'
            assert len(image_data(sent[0])) == 21 and len(names) <= 20, (len(image_data(sent[0])), names)
            refused_round = names
        else:
            assert len(sent) == 1, (index, len(sent))
    omitted = [item['data'] for item in api(f'/threads/{capped}/items') if item['data']['type'] == 'media_omitted']
    assert len(omitted) == 1 and omitted[0].get('oldest') and not omitted[0].get('guessed'), omitted
    left_out = omitted[0]['names']
    assert left_out == [f'shot-{index:02}.png' for index in range(1, len(left_out) + 1)], left_out
    assert 'Too many images' in omitted[0]['reason'], omitted
    results.append({'check': 'a 20-image provider gets the newest after one refusal',
                    'leftOut': left_out, 'retried': refused_round})
    print(f'PASS image count: the oldest {len(left_out)} left out, the newest reached the model', flush=True)

    # 7. The TUI shows both notices in the transcript.
    def pane(name):
        return run(['tmux', '-L', socket, 'capture-pane', '-p', '-J', '-S', '-200', '-t', name])

    def show(name, chat, wanted):
        run(['tmux', '-L', socket, 'new-session', '-d', '-x', '140', '-y', '45', '-s', name,
             e2e_tmux.pane_command(env, args + ['--thread', chat])])
        return until(lambda: (lambda text: text if wanted in text else None)(pane(name)),
                     f'TUI notice {wanted!r} missing', timeout=60)
    screen = show(session, thread, 'The provider refused retina.png')
    (EVIDENCE / 'tui.txt').write_text(screen)
    assert 'left out of this chat from now on' in screen
    screen = show(session + '-capped', capped, f'so the oldest {len(left_out)} images are left out')
    (EVIDENCE / 'tui-oldest.txt').write_text(screen)
    assert 'The request had too many images for the provider' in ' '.join(screen.split()), screen
    results.append({'check': 'TUI transcript shows the notices', 'capture': str(EVIDENCE / 'tui.txt'),
                    'oldest': str(EVIDENCE / 'tui-oldest.txt')})
    print('PASS TUI shows the notices', flush=True)
    (EVIDENCE / 'results.json').write_text(json.dumps(results, indent=2))
    print('evidence:', EVIDENCE)
finally:
    subprocess.run(['tmux', '-L', socket, 'kill-server'], capture_output=True)
    if connection:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    service.shutdown()
    if os.environ.get('HAARNESS_IMAGE_KEEP') == '1':
        print(f'Evidence retained in {root}', flush=True)
    else:
        e2e_scratch.settle(root)
