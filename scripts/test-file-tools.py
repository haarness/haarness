#!/usr/bin/env python3
"""Real-binary proof of the October 24 file-tool fixes, with the echo provider.

1. read: a line that never ends (a 64 MiB one-line bundle) shows its first 2000 characters, the
   server holding no copy of the file; a read past 50 KB stops there and names the offset to go
   on from, with nothing copied to the tool output folder, and a read without a limit says when
   more lines follow; Latin-1 text reads with U+FFFD and a note, and edit refuses it plainly; a
   pipe is refused at once instead of waited on.
2. grep: a long matching line shows its head; the 10,000-line limit holds across files and says
   so.
3. ~: `write ~/Desktop/notes.md` writes in the home folder (an `external_directory` rule written
   with `~` allows it), never `<project>/~`; a `read` rule written with `~` refuses `~/.ssh/…`;
   a project folder named `~` is reached as `./~`.
4. edit: an old text of several lines changes a file with CRLF line ends, which keeps them.
5. rg: a cache folder the bundled rg cannot be put in leaves shell calls running, with one line
   in the log.
6. instructions: a folder's AGENTS.md that read attached comes back with the next read there once
   compaction summarized the result that carried it, or an undo removed it, and not while it is
   still in the chat, also when it is a symlink (to the folder's CLAUDE.md) or, on a filesystem
   that ignores case, named agents.md.
7. read_end, edit_empty: a read past the last line says how long the file is; an edit with an empty
   old says to use write.

Usage: python3 scripts/test-file-tools.py   (HAARNESS_BIN picks the binary, HAARNESS_E2E_ROOT a
folder to keep the fixture in, HAARNESS_E2E_CASES a comma-separated subset of read_line,
read_budget, read_text, read_pipe, grep_line, grep_limit, home, edit_crlf, rg, instructions,
read_end and edit_empty). No model is
called.
"""
import json
import os
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-file-tools-')
root = Path(keep or holder.name).resolve()
root.mkdir(parents=True, exist_ok=True)
home, data = root / 'home', root / 'data'
home.mkdir(exist_ok=True)
env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
       'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_CLOUD_API_URL': 'http://127.0.0.1:9'}
TOOL_OUTPUT = data / 'data' / 'tool-output'
CUT = '… (line truncated to 2000 characters)'
servers = []
checks = []


def project(name, config=None):
    folder = root / name
    folder.mkdir(parents=True, exist_ok=True)
    (folder / 'haarness.json').write_text(json.dumps({'model': 'echo/echo-1', **(config or {})}))
    subprocess.run(['git', 'init', '-q', str(folder)], check=True)
    return folder


def haarness(folder, prompt, environment=None, timeout=120):
    environment = environment or env
    if (folder, environment['HAARNESS_DATA_DIR']) not in [(f, e['HAARNESS_DATA_DIR']) for f, e in servers]:
        servers.append((folder, environment))
    result = subprocess.run([str(BINARY), '--workspace', str(folder), '--trust-project', 'run', prompt],
                            env=environment, capture_output=True, text=True, timeout=timeout,
                            stdin=subprocess.DEVNULL)
    return result.stdout + result.stderr


def tool(folder, name, arguments, **kwargs):
    return haarness(folder, f'!tool {name} {json.dumps(arguments)}', **kwargs)


def passed(label):
    checks.append(label)
    print('PASS', label, flush=True)


def saved_outputs():
    return set(TOOL_OUTPUT.glob('*')) if TOOL_OUTPUT.exists() else set()


def server_pids(folder):
    """The folder's own server: a process started for it, not this script's `run` client."""
    listing = subprocess.run(['ps', '-axo', 'pid=,command='], capture_output=True, text=True).stdout
    return [int(line.split()[0]) for line in listing.splitlines()
            if str(BINARY) in line and str(folder) in line and ' run ' not in line]


def peak_rss(folder, action):
    """Run `action` while sampling the folder's server memory; the largest RSS seen, in MiB."""
    peak, done = [0], threading.Event()

    def sample():
        while not done.is_set():
            for pid in server_pids(folder):
                rss = subprocess.run(['ps', '-o', 'rss=', '-p', str(pid)], capture_output=True, text=True).stdout
                if rss.strip():
                    peak[0] = max(peak[0], int(rss) // 1024)
            time.sleep(.02)

    sampler = threading.Thread(target=sample)
    sampler.start()
    try:
        result = action()
    finally:
        done.set()
        sampler.join()
    return result, peak[0]


files = project('files', {'permission': {'external_directory': {'~/*': 'allow'},
                                          'read': {'~/.ssh/*': 'deny'}}})


def case_read_line():
    with open(files / 'bundle.min.js', 'wb') as bundle:
        chunk = b'x' * (1024 * 1024)
        for _ in range(64):
            bundle.write(chunk)
        bundle.write(b'\nend\n')
    tool(files, 'read', {'path': 'haarness.json'})  # the server is up before memory is sampled
    before = saved_outputs()
    output, peak = peak_rss(files, lambda: tool(files, 'read', {'path': 'bundle.min.js'}))
    assert 'x' * 2000 + CUT in output and 'x' * 2001 not in output and 'end' in output, output[:300]
    assert saved_outputs() == before, 'a read copied the file to the tool output folder'
    print(f'  server peak RSS during the read: {peak} MiB', flush=True)
    assert 0 < peak < 200, f'the server held the 64 MiB line: {peak} MiB'
    passed(f'a 64 MiB one-line file reads as its first 2000 characters (server peak {peak} MiB)')


def case_read_budget():
    lines = [f'line {index:04} ' + 'z' * 90 for index in range(3000)]
    (files / 'long.txt').write_text('\n'.join(lines))
    before = saved_outputs()
    output = tool(files, 'read', {'path': 'long.txt', 'limit': 3000})
    # `run` prints the result after `Tool read => `.
    shown = [line for line in output.replace('Tool read => ', '\n').splitlines() if line.startswith('line ')]
    count = len(shown)
    assert 400 < count < 3000 and shown[-1] == lines[count - 1], count
    assert f'(stopped at 50 KB; read on with offset={count})' in output, output[-400:]
    assert saved_outputs() == before, 'a long read was copied to the tool output folder'
    output = tool(files, 'read', {'path': 'long.txt', 'offset': count, 'limit': 1})
    assert lines[count] in output and lines[count - 1] not in output, output[-400:]
    passed(f'a read stops at 50 KB after {count} lines and goes on from offset={count}')
    (files / 'short.txt').write_text('\n'.join(str(index) for index in range(2500)))
    output = tool(files, 'read', {'path': 'short.txt'})
    assert '1999\n\n(more lines follow; read on with offset=2000)' in output, output[-300:]
    passed('a read without a limit says when its 2000 lines end before the file does')


def case_read_text():
    (files / 'latin1.txt').write_bytes(b'caf\xe9 au lait\n')
    output = tool(files, 'read', {'path': 'latin1.txt'})
    assert 'caf� au lait' in output, output
    assert 'this file is not UTF-8' in output and 'write would replace those bytes' in output, output
    output = tool(files, 'edit', {'path': 'latin1.txt', 'old': 'au', 'new': 'with'})
    assert 'is not UTF-8 text, so edit cannot change it' in output, output
    assert (files / 'latin1.txt').read_bytes() == b'caf\xe9 au lait\n'
    passed('a Latin-1 file reads with U+FFFD and a note, and edit refuses it plainly')
    (files / 'wide.txt').write_text('日' * 3000 + '\nend\n')
    output = tool(files, 'read', {'path': 'wide.txt'})
    assert '日' * 2000 + CUT in output and 'not UTF-8' not in output and '\ufffd' not in output, output[-300:]
    passed('a long line of CJK text reads as UTF-8, its cut leaving no U+FFFD')


def case_read_pipe():
    pipe = files / 'pipe'
    if not pipe.exists():
        os.mkfifo(pipe)
    started = time.monotonic()
    output = tool(files, 'read', {'path': 'pipe'}, timeout=60)
    assert 'is not a regular file' in output, output
    passed(f'a read of a pipe is refused in {time.monotonic() - started:.1f} s instead of waiting')


def case_grep_line():
    (files / 'grep').mkdir(exist_ok=True)
    (files / 'grep' / 'app.min.js').write_text('needle' + 'q' * 100_000 + '\nshort needle\n')
    before = saved_outputs()
    output = tool(files, 'grep', {'pattern': 'needle', 'path': 'grep'})
    assert 'grep/app.min.js:1:needle' + 'q' * 1994 + CUT in output, output[:300]
    assert 'grep/app.min.js:2:short needle' in output, output
    assert saved_outputs() == before, 'a grep of one long line filled the output'
    passed('grep shows the first 2000 characters of a long matching line')


def case_grep_limit():
    folder = files / 'many'
    folder.mkdir(exist_ok=True)
    for name in 'abc':
        (folder / f'{name}.txt').write_text('hit\n' * 6000)
    before = saved_outputs()
    output = tool(files, 'grep', {'pattern': 'hit', 'path': 'many'})
    assert '(stopped at 10000 matching lines; narrow the pattern or the path)' in output, output[-400:]
    saved = sorted(saved_outputs() - before)
    assert len(saved) == 1 and len(saved[0].read_text().splitlines()) == 10_000, saved
    passed('grep stops at 10,000 matching lines across files and says so')


def case_home():
    output = tool(files, 'write', {'path': '~/Desktop/notes.md', 'content': 'from the agent\n'})
    assert (home / 'Desktop' / 'notes.md').read_text() == 'from the agent\n', output
    assert not (files / '~').exists(), 'a folder named ~ was made in the project'
    output = tool(files, 'read', {'path': '~/Desktop/notes.md'})
    assert 'from the agent' in output, output
    passed('~/Desktop/notes.md is written and read in the home folder, never <project>/~')
    (home / '.ssh').mkdir(exist_ok=True)
    (home / '.ssh' / 'id_rsa').write_text('PRIVATE-KEY\n')
    output = tool(files, 'read', {'path': '~/.ssh/id_rsa'})
    assert 'PRIVATE-KEY' not in output and f"denied by permission rule 'read:{home}/.ssh/*'" in output, output
    passed('a read rule written with ~ refuses ~/.ssh/id_rsa')
    (files / '~').mkdir(exist_ok=True)
    (files / '~' / 'draft.md').write_text('project draft\n')
    output = tool(files, 'read', {'path': './~/draft.md'})
    assert 'project draft' in output, output
    passed('a project folder named ~ is read as ./~')


def case_edit_crlf():
    (files / 'main.c').write_bytes(b'int a() {\r\n    one();\r\n}\r\n')
    output = tool(files, 'edit', {'path': 'main.c', 'old': '    one();\n}', 'new': '    two();\n    three();\n}'})
    assert (files / 'main.c').read_bytes() == b'int a() {\r\n    two();\r\n    three();\r\n}\r\n', output
    passed('an edit of several lines changes a CRLF file and keeps its line ends')


def case_rg():
    cache_data = root / 'data-rg'
    (cache_data / 'cache').mkdir(parents=True, exist_ok=True)
    # The bundled rg's folder cannot be made: its name is taken by a file.
    (cache_data / 'cache' / 'ripgrep').write_text('not a folder\n')
    environment = dict(env, HAARNESS_DATA_DIR=str(cache_data))
    folder = project('rg')
    for _ in range(2):
        output = tool(folder, 'bash', {'command': 'printf shell-ran'}, environment=environment)
        assert 'shell-ran' in output, output
    logs = [line for path in cache_data.rglob('haarness.log*') for line in path.read_text(errors='replace').splitlines()
            if 'the bundled ripgrep cannot be extracted' in line]
    assert len(logs) == 1, logs
    passed('shell calls run when the bundled rg cannot be extracted, said once in the log')


def case_instructions():
    folder = project('instructions')
    (folder / 'sub').mkdir(exist_ok=True)
    (folder / 'sub' / 'AGENTS.md').write_text('SUB_FOLDER_RULES\n')
    for name in 'ab':
        (folder / 'sub' / f'{name}.txt').write_text(f'{name} text\n')
    servers.append((folder, env))
    connection = json.loads(subprocess.run(
        [str(BINARY), '--workspace', str(folder), '--trust-project', 'desktop'], input='{"action":"connect"}',
        env=env, capture_output=True, text=True, timeout=90, check=True).stdout)

    def api(path, body=None):
        request = urllib.request.Request(
            connection['url'] + '/v1' + path, data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    def turn(thread, text):
        api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})
        for _ in range(600):
            if not any(active.get('threadId') == thread for active in api('/turns/active')):
                return
            time.sleep(.05)
        raise AssertionError('the turn did not finish')

    def reads(thread, path):
        """What the model's read of `path` attached."""
        turn(thread, '!tool read ' + json.dumps({'path': path}))
        result = [item['data'] for item in api(f'/threads/{thread}/items')
                  if item['data']['type'] == 'tool_result' and item['data']['name'] == 'read'][-1]
        loaded = (result.get('metadata') or {}).get('loaded') or []
        assert ('SUB_FOLDER_RULES' in result['output']) == bool(loaded), result['output']
        return loaded

    def chat():
        return api('/threads', {'title': 'Nested instructions', 'provider': 'echo', 'model': 'echo-1'})['id']

    rules = [str((folder / 'sub' / 'AGENTS.md').resolve())]
    thread = chat()
    assert reads(thread, 'sub/a.txt') == rules
    assert reads(thread, 'sub/b.txt') == [], 'attached again while still in the chat'
    for text in ['First other question', 'Second other question']:
        turn(thread, text)
    # The two most recent turns stay behind the summary; both reads are summarized.
    api(f'/threads/{thread}/compact', {'summary': 'Earlier work, summarized.'})
    assert reads(thread, 'sub/b.txt') == rules, 'not attached after compaction'
    assert reads(thread, 'sub/a.txt') == []
    passed('a folder\'s AGENTS.md comes back after compaction, and only once while it is in the chat')
    thread = chat()
    turn(thread, 'Hello')
    assert reads(thread, 'sub/a.txt') == rules
    api(f'/threads/{thread}/revert', {})
    assert reads(thread, 'sub/b.txt') == rules, 'not attached after the undo'
    passed('a folder\'s AGENTS.md comes back after the turn that brought it was undone')
    (folder / 'linked').mkdir(exist_ok=True)
    (folder / 'linked' / 'CLAUDE.md').write_text('SUB_FOLDER_RULES\n')
    os.symlink('CLAUDE.md', folder / 'linked' / 'AGENTS.md')
    (folder / 'cased').mkdir(exist_ok=True)
    (folder / 'cased' / 'agents.md').write_text('SUB_FOLDER_RULES\n')
    for sub in ('linked', 'cased'):
        for name in 'ab':
            (folder / sub / f'{name}.txt').write_text(f'{name} text\n')
    shapes = ['linked'] + (['cased'] if (folder / 'cased' / 'AGENTS.md').is_file() else [])
    for sub in shapes:
        thread = chat()
        assert reads(thread, f'{sub}/a.txt') == [str(folder.resolve() / sub / 'AGENTS.md')]
        assert reads(thread, f'{sub}/b.txt') == [], f'{sub}: attached again while still in the chat'
    passed(f'a folder\'s AGENTS.md that is a symlink{" or named agents.md" if "cased" in shapes else ""} '
           'is attached once while it is in the chat')


def case_read_end():
    (files / 'three.txt').write_text('one\ntwo\nthree\n')
    output = tool(files, 'read', {'path': 'three.txt', 'offset': 7})
    assert '(the file has 3 lines, so offset 7 is past its end)' in output, output
    passed('a read past the last line says how many lines the file has')


def case_edit_empty():
    (files / 'notes.md').write_text('# Notes\n')
    output = tool(files, 'edit', {'path': 'notes.md', 'old': '', 'new': 'x'})
    assert 'old is empty: give the exact text to replace, or use write' in output, output
    assert (files / 'notes.md').read_text() == '# Notes\n'
    passed('an edit with an empty old says to use write and leaves the file alone')


CASES = {name: globals()['case_' + name] for name in ['read_line', 'read_budget', 'read_text', 'read_pipe',
                                                       'grep_line', 'grep_limit', 'home', 'edit_crlf', 'rg',
                                                       'instructions', 'read_end', 'edit_empty']}
selected = [name for name in os.environ.get('HAARNESS_E2E_CASES', ','.join(CASES)).split(',') if name]
try:
    for name in selected:
        CASES[name]()
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
    print(f'{len(checks)} checks passed', flush=True)
finally:
    for folder, environment in servers:
        subprocess.run([str(BINARY), '--workspace', str(folder), 'serve', '--stop'], env=environment,
                       capture_output=True, timeout=60)
    if holder:
        holder.cleanup()
