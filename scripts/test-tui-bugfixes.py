#!/usr/bin/env python3
"""Real tmux regressions for TUI drafts, input integrity, shortcuts, selectors, and code's colours and tabs."""
import concurrent.futures
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time
import urllib.request

import e2e_scratch
import e2e_tmux

repo = Path(__file__).resolve().parents[1]
binary = str(Path(os.environ.get('HAARNESS_BIN', repo / 'target/debug/haarness')).resolve())
root = Path(tempfile.mkdtemp(prefix='haarness-tui-regressions-')).resolve()
project = root / 'project'
project.mkdir()
data = root / 'state'
(data / 'config').mkdir(parents=True)
(data / 'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n')
env = {**os.environ, 'HAARNESS_DATA_DIR': str(data), 'HAARNESS_TEST_PROVIDER': '1'}
args = [binary, '--workspace', str(project)]
socket = 'haarness-regressions-' + str(os.getpid())
session = 'main'
checks = []
endpoint = None


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, stderr=subprocess.PIPE, **kwargs)


def tmux(*args, **kwargs):
    return run('tmux', '-L', socket, *args, **kwargs)


def api(path, body=None):
    request = urllib.request.Request(endpoint['url'] + '/v1' + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + endpoint['token'], 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        content = response.read()
        return json.loads(content) if content else None


def until(check, label):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.08)
    raise AssertionError(label)


def pane():
    return tmux('capture-pane', '-p', '-t', session)


def cells(row):
    """The characters of a row captured with `-e`, each with the foreground it is drawn in."""
    result, colour = [], None
    for part in re.split(r'(\x1b\[[0-9;]*m)', row):
        if not part.startswith('\x1b['):
            result += [(character, colour) for character in part]
            continue
        codes, index = (part[2:-1] or '0').split(';'), 0
        while index < len(codes):
            if codes[index] in ('38', '48') and codes[index + 1:index + 2] in (['2'], ['5']):
                step = 5 if codes[index + 1] == '2' else 3
                if codes[index] == '38':
                    colour = tuple(codes[index + 2:index + step])
                index += step
                continue
            if codes[index] in ('0', '39'):
                colour = None
            index += 1
    return result


def code_row(needle):
    """The drawn row holding `needle` outside a sent prompt, as (text, foreground of each character)."""
    for row in tmux('capture-pane', '-p', '-e', '-t', session).splitlines():
        drawn = cells(row)
        plain = ''.join(character for character, _ in drawn)
        if needle in plain and not plain.lstrip().startswith('▎'):
            return plain, [colour for _, colour in drawn]
    return None


def colour_of(row, needle):
    """The foreground of `needle`'s last place on `row`: in a side-by-side diff, the new side."""
    plain, colours = row
    return colours[plain.rindex(needle)]


def keys(*values):
    tmux('send-keys', '-t', session, *values)
    time.sleep(.12)


def text(value):
    tmux('send-keys', '-l', '-t', session, '--', value)
    time.sleep(.12)


def paste(value):
    tmux('load-buffer', '-b', 'fixture', '-', input=value)
    tmux('paste-buffer', '-r', '-p', '-b', 'fixture', '-t', session)
    time.sleep(.2)


def launch(thread):
    subprocess.run(['tmux', '-L', socket, 'kill-session', '-t', session], capture_output=True)
    command = shlex.join(['env', 'HAARNESS_DATA_DIR=' + str(data), 'HAARNESS_TEST_PROVIDER=1', *args, '--thread', thread])
    tmux('new-session', '-d', '-x', '120', '-y', '36', '-s', session, command)
    until(lambda: 'h a a r n e s s' in pane(), 'TUI did not launch')


def new(title):
    thread = api('/threads', {'title': title, 'provider': 'echo', 'model': 'echo-1'})['id']
    launch(thread)
    return thread


def submit(thread):
    before = len(api('/threads/' + thread + '/items'))
    keys('Enter')
    records = until(lambda: [i for i in api('/threads/' + thread + '/items')[before:] if i['data']['type'] == 'user_message'], 'message was not submitted')
    until(lambda: not api('/turns/active'), 'turn remained active')
    return records[0]['data']['text']


def check(name, success):
    (root / (name + '.txt')).write_text(pane())
    (root / (name + '.ansi')).write_text(tmux('capture-pane', '-p', '-e', '-t', session))
    checks.append({'name': name, 'passed': bool(success)})
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
    assert success, name
    print('PASS', name, flush=True)


def quit_client(target):
    tmux('send-keys', '-t', target, 'C-p')
    time.sleep(.15)
    tmux('send-keys', '-l', '-t', target, 'quit')
    tmux('send-keys', '-t', target, 'Enter')
    until(lambda: subprocess.run(['tmux', '-L', socket, 'has-session', '-t', target], capture_output=True).returncode != 0, 'client did not quit')


print('Evidence:', root, flush=True)
try:
    run('git', 'init', '-q', str(project))
    endpoint = json.loads(run(*args, 'desktop', env=env, input='{"action":"connect"}', timeout=90))
    payload = 'line\n' * 12
    for name, edits in [('paste-backspace', ['BSpace']), ('paste-delete', ['Home', 'Delete']), ('paste-word-delete', ['C-w'])]:
        thread = new(name)
        paste(payload)
        keys(*edits)
        text('after')
        check(name, submit(thread) == 'after')
    thread = new('literal-paste-markers')
    first = 'FIRST [#2 pasted 1 lines] ' + 'a' * 1100
    second = 'b' * 1100
    paste(first)
    text(' | ')
    paste(second)
    check('literal-paste-markers', submit(thread) == first + ' | ' + second)
    for index, value in enumerate(['e\u0301', '👍🏽', '🇯🇵', '👨‍👩‍👧‍👦']):
        thread = new('unicode-' + str(index))
        paste('A' + value)
        keys('BSpace')
        check('unicode-' + str(index), submit(thread) == 'A')
    thread = new('model-shortcut')
    paste('DRAFT-F2')
    before = len(api('/threads/' + thread + '/items'))
    keys('F2')
    until(lambda: 'Select model' in pane(), 'F2 did not open models')
    check('f2-preserves-unsent-draft', len(api('/threads/' + thread + '/items')) == before and 'DRAFT-F2' in pane())
    keys('Escape')
    check('enter-still-submits', submit(thread) == 'DRAFT-F2')
    # A program before the TUI left the keypad in application mode (ESC =), where tmux sends
    # keypad digits as ESC O q, ESC O r, which typed nothing: the TUI's own numeric mode makes
    # them type. (tmux sends the keypad's Enter as a line feed in numeric mode, which reads as
    # Ctrl+J, so the main Enter sends here.)
    thread = api('/threads', {'title': 'keypad', 'provider': 'echo', 'model': 'echo-1'})['id']
    subprocess.run(['tmux', '-L', socket, 'kill-session', '-t', session], capture_output=True)
    keypad = ['sh', '-c', 'printf "\\033="; exec "$@"', 'sh', *args, '--thread', thread]
    tmux('new-session', '-d', '-x', '120', '-y', '36', '-s', session, e2e_tmux.pane_command(env, keypad))
    until(lambda: 'h a a r n e s s' in pane(), 'TUI did not launch')
    keys('KP1', 'KP2')
    check('keypad-digits-type', tmux('display', '-p', '-t', session, '#{keypad_flag}').strip() == '0'
          and any(line.strip() == '12' for line in pane().splitlines()[-6:]))
    check('keypad-digits-send', submit(thread) == '12')
    thread = new('normal-draft-mode')
    text('printf THIS-IS-A-CHAT-MESSAGE')
    keys('M-n')
    text('!')
    text('printf SHELL-DRAFT')
    other = next(t for t in api('/threads') if t['id'] != thread and t['title'] in ('New chat', 'New thread'))['id']
    keys('C-l')
    text('normal-draft-mode')
    keys('Enter')
    until(lambda: 'THIS-IS-A-CHAT-MESSAGE' in pane(), 'normal draft did not restore')
    check('normal-draft-mode', submit(thread) == 'printf THIS-IS-A-CHAT-MESSAGE')
    keys('C-l')
    text(other[-8:])
    keys('Enter')
    until(lambda: 'SHELL-DRAFT' in pane(), 'shell draft did not restore')
    check('shell-draft-mode-restores', ' shell ' in pane().splitlines()[-1])
    keys('M-n')
    check('fresh-session-chat-mode', ' shell ' not in pane().splitlines()[-1])
    keys('C-l')
    text('FILTER-' + 'x' * 150 + '-TAIL')
    until(lambda: '-TAIL▌' in pane(), 'search suffix and caret')
    check('search-cursor-visible', '-TAIL▌' in pane())
    keys('Escape')
    # PHP without its `<?php` tag is coloured as PHP: a reply's code fence, and
    # the rows of a `.php` file in /diff, which are drawn one at a time. A
    # template's rows keep reading as markup, so its words stay plain.
    php = project / 'greet.php'
    php.write_text('<?php\n\nfunction greet(string $who): string\n{\n    return $who;\n}\n')
    view = project / 'home.blade.php'
    view.write_text("@extends('layouts.app')\n\n@section('content')\n<div class=\"card\">\n"
                    "    Welcome back, {{ $user->name }}! Don't forget your notes.\n"
                    "    <a href=\"/notes\"\n       class=\"link\">Open your notes</a>\n</div>\n@endsection\n")
    # Tabs open to stops of four, in a code fence and in /diff, with nothing
    # after them drawn over.
    make = project / 'Makefile'
    make.write_text('build:\n\techo "build"\n\tgo vet\t./...\n')
    run('git', '-C', str(project), 'add', 'greet.php', 'home.blade.php', 'Makefile')
    run('git', '-C', str(project), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', 'commit', '-q', '-m', 'PHP fixture')
    php.write_text(php.read_text().replace('    return $who;', '    // Greet by name.\n    return "Hello, " . $who;'))
    view.write_text(view.read_text().replace('Welcome back', 'Welcome home').replace('Open your notes', 'Open the notes'))
    make.write_text(make.read_text().replace('\techo "build"', '\techo "built"\tdone'))
    thread = api('/threads', {'title': 'php-highlight', 'provider': 'echo', 'model': 'echo-1'})['id']
    subprocess.run(['tmux', '-L', socket, 'kill-session', '-t', session], capture_output=True)
    # What a drag copies goes to clipboard tools of the script's own, which
    # keep it in a file: the person's clipboard is never touched.
    clipboard = root / 'clipboard'
    clipboard.mkdir()
    copied = root / 'copied.txt'
    for tool in ('pbcopy', 'wl-copy', 'xclip', 'xsel'):
        (clipboard / tool).write_text(f"#!/bin/sh\ncat > '{copied}'\n")
        (clipboard / tool).chmod(0o755)
    pane_env = {**env, 'COLORTERM': 'truecolor', 'PATH': f"{clipboard}:{os.environ['PATH']}"}
    tmux('new-session', '-d', '-x', '120', '-y', '48', '-s', session,
         e2e_tmux.pane_command(pane_env, [*args, '--thread', thread]))
    until(lambda: 'h a a r n e s s' in pane(), 'TUI did not launch')

    def drag(row, start, end, last_row=None):
        """What a mouse drag from `start` on `row` to `end` on `last_row` copies."""
        copied.unlink(missing_ok=True)
        last_row = row if last_row is None else last_row
        for event in (f'0;{start + 1};{row + 1}M', f'32;{end + 1};{last_row + 1}M',
                      f'32;{end + 1};{last_row + 1}M', f'0;{end + 1};{last_row + 1}m'):
            text('\x1b[<' + event)
        return until(lambda: copied.exists() and copied.read_text(), 'a drag copied nothing')

    def row_of(needle, prompt=False):
        return next(index for index, line in enumerate(pane().splitlines())
                    if needle in line and line.startswith('▎') == prompt)
    paste('PHP:\n```php\n$name = strlen("x"); // why\nreturn "ok";\n```')
    submit(thread)
    row = until(lambda: code_row('return "ok";'), "the reply's code block did not show")
    other = code_row('$name = strlen')
    check('php-fence-coloured',
          len({colour_of(row, 'return'), colour_of(row, '"ok"'), colour_of(row, ';')}) == 3
          and colour_of(other, '// why') not in (colour_of(other, '$name'), colour_of(other, '"x"')))
    paste('Go:\n```go\nfunc main() {\n\tfmt.Println("hi")\t// say it\n}\n```\n```make\nall:\n\techo "build"\n```')
    sent = submit(thread)
    row = until(lambda: code_row('fmt.Println'), "the reply's Go block did not show")
    check('fence-tabs-open', '\t' in sent and '      fmt.Println("hi")   // say it' in row[0])
    # The sent prompt's own block, the draft in the prompt (edited after a
    # tab) and a command's output open their tabs the same way.
    check('prompt-block-tabs-open', any(line.startswith('▎     fmt.Println("hi")   // say it')
                                        for line in pane().splitlines()))
    # A drag copies the tabs themselves: a Makefile copied from a reply runs.
    go = drag(row_of('fmt.Println'), 0, 79)
    make = drag(row_of('all:'), 0, 79, row_of('echo "build"'))
    prompt = drag(row_of('fmt.Println', prompt=True), 0, 79)
    check('copy-keeps-tabs-in-replies-and-prompts',
          go.strip(' ') == '\tfmt.Println("hi")\t// say it' and make.strip(' ') == 'all:\n\techo "build"'
          and prompt.strip(' ') == '\tfmt.Println("hi")\t// say it')
    paste('\tif ok {')
    until(lambda: any(line.startswith('      if ok {') for line in pane().splitlines()), 'the draft did not open its tab')
    keys('Home')
    keys('Right')
    text('X')
    until(lambda: any(line.startswith('      Xif ok {') for line in pane().splitlines()),
          'typing after the tab did not land after it')
    check('draft-tabs-open', True)
    keys('End', 'C-u')
    until(lambda: 'if ok {' not in pane(), 'the draft did not clear')
    text('!')
    text("printf 'col1\\tcol2\\tend\\n'")
    keys('Enter')

    def output_row():
        if 'Allow once' in pane():
            keys('y')
        return any('▏ col1    col2    end ' in line + ' ' for line in pane().splitlines())
    until(output_row, "the command's output did not open its tabs")
    check('output-tabs-open', True)
    until(lambda: not api('/turns/active'), 'the command did not finish')
    check('copy-keeps-tabs-in-output', drag(row_of('▏ col1'), 0, 79).strip(' ') == 'col1\tcol2\tend')
    text('/diff')
    keys('Enter')
    until(lambda: code_row('echo "built"'), '/diff did not show the Makefile change')
    tabbed = ('    echo "build"', '    echo "built"    done', '    go vet  ./...')
    check('diff-tabs-open', all(code_row(code) for code in tabbed))
    vet = row_of('go vet')
    at = pane().splitlines()[vet].rindex('go vet') - 4
    check('copy-keeps-tabs-in-diff', drag(vet, at, at + 30).strip(' ') == '\tgo vet\t./...')

    def side_by_side():
        return any('echo "build"' in line and 'echo "built"' in line for line in pane().splitlines())
    # The same rows in the other layout (side by side ⇄ stacked), then back.
    first = side_by_side()
    keys('v')
    until(lambda: side_by_side() != first and all(code_row(code) for code in tabbed),
          'the other layout did not show or drew a tab wrong')
    check('diff-tabs-open-other-layout', True)
    keys('v')
    until(lambda: side_by_side() == first, 'the layout did not switch back')
    row = until(lambda: code_row('return "Hello, "'), '/diff did not show the PHP change')
    comment = code_row('// Greet by name.')
    check('php-diff-rows-coloured',
          len({colour_of(row, 'return'), colour_of(row, '"Hello, "'), colour_of(row, '$who')}) == 3
          and colour_of(comment, '// Greet') != colour_of(row, '$who'))

    def one_colour(needle):
        plain, colours = until(lambda: code_row(needle), f'/diff did not show {needle!r}')
        start = plain.rindex(needle)
        return len(set(colours[start:start + len(needle)])) == 1
    check('php-template-rows-plain', all(one_colour(needle) for needle in (
        'Welcome home, {{ $user->name }}!', 'class="link">Open the notes</a>', '@endsection')))
    keys('q')
    # Both clients load before either saves. Exit concurrently to exercise the lock.
    session = 'a'
    a = new('parallel-a')
    paste('KEEP-A')
    session = 'b'
    b = new('parallel-b')
    paste('KEEP-B')
    with concurrent.futures.ThreadPoolExecutor(2) as pool:
        list(pool.map(quit_client, ['a', 'b']))
    session = 'a'
    launch(a)
    check('concurrent-save-retains-a', 'KEEP-A' in pane())
    quit_client('a')
    session = 'b'
    launch(b)
    check('concurrent-save-retains-b', 'KEEP-B' in pane())
    quit_client('b')
    old_url = endpoint['url']
    run(*args, 'serve', '--stop', env=env, timeout=30)
    endpoint = json.loads(run(*args, 'desktop', env=env, input='{"action":"connect"}', timeout=90))
    session = 'a'
    launch(a)
    check('daemon-restart-retains-draft', 'KEEP-A' in pane())
    quit_client('a')
    # Exercise migration from the legacy tuple format at a previous daemon URL.
    draft_file = data / 'config/tui-server-drafts.json'
    draft_file.write_text(json.dumps({old_url: {'thread': a, 'drafts': {a: ['LEGACY-NORMAL', []], b: ['LEGACY-SHELL', []]}, 'shell': False}}))
    launch(a)
    check('legacy-port-and-format-migration', 'LEGACY-NORMAL' in pane() and ' shell ' not in pane().splitlines()[-1])
    quit_client('a')
    (data / 'config/tui-state.toml').write_text('[keybindings]\nmodels="ctrl+m"\nsessions="F2"\n')
    launch(a)
    keys('F3')
    check('legacy-shortcut-migration-respects-custom-f2', 'Select model' in pane() and 'LEGACY-NORMAL' in pane())
    print('PASS:', len(checks), 'TUI bugfix regressions', flush=True)
finally:
    subprocess.run(['tmux', '-L', socket, 'kill-server'], capture_output=True)
    if endpoint:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
    e2e_scratch.settle(root)
