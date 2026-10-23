#!/usr/bin/env python3
"""Real attached TUI drive: a shell request that runs a script shows the script on the approval card.

The card names the script and its size under the command and shows its first lines; ctrl+e expands
it to read it whole, a file read only in part says so at its end, and a secret-named file or a
command that runs no script shows nothing. `HAARNESS_TUI_VARIANT=light|dark` picks the theme, and
each check also keeps a colored capture (`.ansi`) beside its text, for screenshots.
"""
import json, os, pathlib, shlex, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch
from e2e_tmux import pane_command

root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-script-tmux-')).resolve()
workspace = root / 'project'
(workspace / '.haarness').mkdir(parents=True)
(workspace / '.haarness/config.toml').write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n[execution]\nshell_timeout_ms=0\n'
)
(workspace / 'scripts').mkdir()
deploy = [
    '#!/bin/sh',
    '# Build the site and publish it.',
    'set -eu',
    'out="${OUT:-dist}"',
    'rm -rf "$out" && mkdir -p "$out"',
    'for page in index about; do',
    '\tprintf "<h1>%s</h1>\\n" "$page" > "$out/$page.html"',
    'done',
    'echo "would upload $out/index.html with --fail --retry 3 to '
    'https://deploy.example.test/sites/haarness-fixture/releases/current/index.html"',
    'printf DEPLOYED > deployed.txt',
    'echo "published $(ls "$out" | wc -l) pages"',
    'exit 0',
]
(workspace / 'scripts/deploy.sh').write_text('\n'.join(deploy) + '\n')
# A script longer than the 30,000 bytes a request carries.
(workspace / 'scripts/big.sh').write_text(
    ''.join(f'echo big-{line:04} {"x" * 100}\n' for line in range(1, 401))
)
(workspace / '.env.local').write_text('TOKEN=fixture-only\n')
(workspace / '.envrc').write_text('export TOKEN=fixture-only\n')
variant = os.environ.get('HAARNESS_TUI_VARIANT')
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve())
env = {k: v for k, v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(
    HAARNESS_HOME=str(root / 'home'),
    HAARNESS_DATA_DIR=str(root / 'data'),
    HAARNESS_TEST_PROVIDER='1',
    HAARNESS_SERVER_TOKEN='',
    HAARNESS_SERVER_PASSWORD='',
)
if variant:
    (root / 'data/config').mkdir(parents=True)
    (root / 'data/config/tui-state.toml').write_text(f'theme = "haarness"\nvariant = "{variant}"\n')
name = 'haarness-script-' + uuid.uuid4().hex[:10]
checks = []
connection = None


def cli(*args, input=None):
    return subprocess.run(
        [binary, '--workspace', str(workspace), *args],
        input=input, env=env, text=True, capture_output=True, check=True, timeout=60,
    ).stdout


def api(path, body=None):
    request = urllib.request.Request(
        connection['url'] + '/v1' + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + connection.get('token', ''), 'Content-Type': 'application/json'},
    )
    with urllib.request.urlopen(request, timeout=10) as reply:
        data = reply.read()
        return json.loads(data) if data else None


def tmux(*args):
    return subprocess.run(['tmux', *args], text=True, capture_output=True, check=True, timeout=10).stdout


def capture(color=False):
    return tmux('capture-pane', '-p', *(['-e'] if color else []), '-t', name + ':0.0')


def until(fn, label, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = fn()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(label + '\n' + capture())


def keys(*args):
    tmux('send-keys', '-t', name, *args)


def check(label, slug):
    checks.append(label)
    (root / f'{len(checks)}-{slug}.txt').write_text(capture())
    (root / f'{len(checks)}-{slug}.ansi').write_text(capture(color=True))
    print('PASS ' + label, flush=True)


def turn(command):
    return api(f'/threads/{thread["id"]}/turns', {
        'requestId': str(uuid.uuid4()),
        'text': '!tool bash ' + json.dumps({'command': command}),
    })


def finish(started):
    result = until(
        lambda: (value if (value := api('/turns/' + started['id']))['status'] not in ['running', 'queued'] else None),
        'turn completion',
    )
    assert result['status'] == 'completed', result


def approval():
    until(lambda: api('/permissions'), 'pending permission')
    until(lambda: 'Allow once' in capture(), 'approval visible')
    return api('/permissions')[0]


try:
    connection = json.loads(cli('desktop', input='{"action":"connect"}'))
    env['HAARNESS_SERVER_TOKEN'] = connection.get('token', '')
    thread = api('/threads', {'title': 'Script on the card', 'provider': 'echo', 'model': 'echo-1'})
    command = [binary, '--workspace', str(workspace), 'attach', connection['url'], '--thread', thread['id']]
    tmux('new-session', '-d', '-s', name, '-x', '110', '-y', '40', pane_command(env, command))
    until(lambda: 'Echo 1' in capture(), 'TUI attach')

    started = turn('cd scripts && sh deploy.sh')
    pending = approval()
    assert pending['script']['path'] == str(workspace / 'scripts/deploy.sh'), pending
    screen = until(lambda: (text if 'Script:' in (text := capture()) else None), 'script row')
    assert 'Script: scripts/deploy.sh · 12 lines' in screen, screen
    assert '│ #!/bin/sh' in screen and '│ # Build the site' in screen, screen
    assert 'exit 0' not in screen and 'ctrl+e expand' in screen, screen
    check('the card names the script under the command and shows its first lines', 'collapsed')
    keys('C-e')
    screen = until(lambda: (text if 'exit 0' in (text := capture()) else None), 'expanded script')
    # The long line is wrapped whole, its tab drawn as spaces.
    assert 'releases/current/index.html' in screen, screen
    assert '│     printf "<h1>' in screen, screen
    assert 'ctrl+e collapse' in screen and 'Allow once' in screen, screen
    check('ctrl+e expands the card to read the whole script, answers still in view', 'expanded')
    keys('y')
    finish(started)
    assert (workspace / 'scripts/deployed.txt').read_text() == 'DEPLOYED'
    check('the approved command runs the script it showed', 'ran')

    started = turn('sh scripts/big.sh')
    approval()
    # The count of a cut file is of what was read.
    until(lambda: 'Script: scripts/big.sh · first ' in capture(), 'cut script size')
    check('a cut script counts only the lines that were read', 'cut-size')
    keys('C-e')
    for _ in range(60):
        keys('PageDown')
    screen = until(
        lambda: (text if 'Only the start of the file is shown.' in (text := capture()) else None),
        'cut note',
    )
    assert 'big-0400' not in screen, screen
    check('a script longer than the request carries says only its start is shown', 'cut')
    keys('n')
    finish(started)

    for command in ['source .env.local', 'source ./.envrc', 'printf plain']:
        started = turn(command)
        pending = approval()
        assert 'script' not in pending, pending
        screen = capture()
        assert 'Script:' not in screen and command in screen, screen
        keys('n')
        finish(started)
    check('a secret-named file, a credential file or a command that runs no script shows nothing more', 'none')
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
    print('PASS', len(checks), 'script card scenarios.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', name], capture_output=True)
    try:
        cli('serve', '--stop')
    except Exception:
        pass
    e2e_scratch.settle(root)
