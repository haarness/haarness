#!/usr/bin/env python3
"""Real Haarness server + real OpenTUI in tmux: Auto asks a reviewer model.

The deterministic test provider's reviewer answers each request by the
`[review:<verdict>]` marker after the scripted call (allow when there is
none), so every outcome is driven end to end: an approval runs the call, an
ask shows the card with the reviewer's note, a block leaves a quiet row with
its reason, a slow review reads `reviewing…`, a failed one asks with a note,
an ask a person configured never reaches the reviewer, a switch to Review or
Stop mid-review settles the request at once, and `haarness run --auto` asks
at a terminal with the note, or without one refuses each such request with
its reason and goes on (exit status 3), stopping after five in a row.

HAARNESS_E2E_FRAMES names a folder for captured frames (ANSI and text);
by default they land beside the other evidence."""
import json, os, pathlib, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch
from e2e_tmux import pane_command

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/release/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-auto-review-e2e-')).resolve()
workspace = root / 'workspace'
(workspace / '.haarness').mkdir(parents=True)
# A whole-tool ask is reviewed; the narrow ask for `printf mine*` is the person's.
(workspace / '.haarness' / 'config.toml').write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n'
    '[permissions]\nbash = { "*" = "ask", "printf mine*" = "ask" }\n'
)
env = dict(
    os.environ,
    HAARNESS_HOME=str(root / 'home'),
    HAARNESS_DATA_DIR=str(root / 'data'),
    HAARNESS_TEST_PROVIDER='1',
    HAARNESS_DEFAULT_PROVIDER='echo',
    HAARNESS_DEFAULT_MODEL='echo-1',
)
env.pop('HAARNESS_TEST_AUTO_REVIEW', None)
name = 'haarness-auto-review-' + str(os.getpid())
endpoint = None
checks = []
frames = pathlib.Path(os.environ.get('HAARNESS_E2E_FRAMES') or root / 'frames')
frames.mkdir(parents=True, exist_ok=True)


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout


def connect():
    return json.loads(run([str(binary), '--workspace', str(workspace), '--trust-project', 'desktop'],
                          env=env, input='{"action":"connect"}', timeout=120))


def api(path, value=None, method=None):
    request = urllib.request.Request(
        endpoint['url'] + path,
        data=None if value is None else json.dumps(value).encode(),
        headers={'Authorization': 'Bearer ' + endpoint['token'], 'Content-Type': 'application/json'},
        method=method,
    )
    with urllib.request.urlopen(request, timeout=10) as reply:
        body = reply.read()
        return json.loads(body) if body else None


def capture(window=0):
    return run(['tmux', 'capture-pane', '-p', '-t', f'{name}:{window}.0'])


def until(check, label, seconds=25, window=0):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if check():
            return
        time.sleep(.08)
    try:
        pane = capture(window)
    except subprocess.CalledProcessError:
        pane = '(no pane)'
    raise AssertionError(label + '\n' + pane)


def keys(*pressed, window=0):
    run(['tmux', 'send-keys', '-t', f'{name}:{window}.0', *pressed])


def command(text):
    keys('C-u')
    run(['tmux', 'send-keys', '-l', '-t', f'{name}:0.0', text])
    # Enter before the last keys are drawn can complete a shorter command.
    until(lambda: text in capture(), 'typed command not shown: ' + text)
    keys('Enter')


def save(label, window=0):
    """Keep what the pane shows, colours included, for a person to look at."""
    (frames / (label + '.ansi')).write_text(run(['tmux', 'capture-pane', '-p', '-e', '-t', f'{name}:{window}.0']))
    (frames / (label + '.txt')).write_text(capture(window))


def start(thread, command, verdict=None):
    text = '!tool bash ' + json.dumps({'command': command}) + (f' [review:{verdict}]' if verdict else '')
    return api('/v1/threads/' + thread + '/turns', {'requestId': str(uuid.uuid4()), 'text': text})


def finished():
    until(lambda: not api('/v1/turns/active'), 'turn did not finish')


def pending():
    return api('/v1/permissions')


def check(label):
    checks.append(label)
    print('PASS ' + label, flush=True)


def never_reviewing(label):
    """Frames sampled until the turn ends: a quick review never says reviewing…."""
    end = time.monotonic() + 25
    while time.monotonic() < end:
        pane = capture()
        assert 'reviewing…' not in pane, label + ' showed reviewing…\n' + pane
        if not api('/v1/turns/active'):
            return
        time.sleep(.03)
    raise AssertionError(label + ' did not finish')


def card(pane):
    """The approval card's rows, from its ⚠ title to its answers."""
    rows = pane.split('\n')
    tops = [index for index, row in enumerate(rows) if '⚠ ' in row]
    if not tops:
        return ''
    end = next((index for index in range(tops[-1], len(rows)) if 'Allow once' in rows[index]), len(rows) - 1)
    return '\n'.join(rows[tops[-1]:end + 1])


try:
    endpoint = connect()
    info = api('/v1/info')
    assert 'approval-review.v1' in info['capabilities'], info['capabilities']
    doctor = run([str(binary), '--workspace', str(workspace), 'doctor'], env=env, timeout=60)
    assert 'auto review: each chat\'s own model, timeout 60s' in doctor, doctor
    check('the server says Auto asks a reviewer, and doctor names the model it asks')

    thread = api('/v1/threads', {'title': 'Auto review verification', 'provider': 'echo', 'model': 'echo-1'})['id']
    api('/v1/threads/' + thread + '/approval-mode', {'mode': 'auto'}, 'PUT')
    run(['tmux', 'new-session', '-d', '-x', '150', '-y', '40', '-s', name,
         pane_command(env, [binary, '--workspace', workspace, '--trust-project', '--thread', thread])])
    until(lambda: '  auto' in capture(), 'the TUI did not open in Auto')

    start(thread, 'printf allowed > allowed.txt')
    never_reviewing('a quick approval')
    until(lambda: (workspace / 'allowed.txt').exists(), 'the reviewer did not approve')
    finished()
    assert not pending()
    check('an approval runs the call once without a prompt, and a quick review never reads reviewing…')

    start(thread, 'printf asked > asked.txt', 'ask')
    until(lambda: len(pending()) == 1, 'the ask was not shown')
    note = pending()[0].get('review') or {}
    assert note.get('status') == 'asked' and note.get('rationale') == 'Test verdict: ask.', note
    until(lambda: 'Auto review · Test rule: Test verdict: ask.' in card(capture()), 'the card did not show the note')
    assert 'Once + Auto' not in card(capture()), 'the chat is already in Auto: ' + card(capture())
    save('ask-card')
    time.sleep(.3)
    keys('y')
    until(lambda: (workspace / 'asked.txt').exists(), 'y did not run the asked call')
    finished()
    check('an ask shows the ordinary card with the reviewer\'s note, and the person decides')

    start(thread, 'printf blocked > blocked.txt', 'deny')
    never_reviewing('a quick block')
    finished()
    assert not (workspace / 'blocked.txt').exists()
    # The row; the echo provider's reply below it quotes the text the model got.
    until(lambda: "⊘ bash printf blocked > blocked.txt  Stopped by Auto's reviewer: Test verdict: deny." in capture(),
          'the row did not say why it was blocked')
    save('blocked-row')
    check("a block refuses the call and its row reads Stopped by Auto's reviewer with the reason")

    start(thread, 'printf slow > slow.txt', 'slow')
    until(lambda: 'reviewing…' in capture(), 'a slow review did not say so', seconds=5)
    save('reviewing')
    until(lambda: (workspace / 'slow.txt').exists(), 'the slow review did not approve')
    finished()
    check('a review longer than a moment reads reviewing… in the activity text, then the call runs')

    start(thread, 'printf failed > failed.txt', 'fail')
    until(lambda: len(pending()) == 1, 'a failed review did not ask')
    assert (pending()[0].get('review') or {}).get('status') == 'failed'
    until(lambda: 'Auto review unavailable: The review failed (' in card(capture()), 'the card did not say the review was unavailable')
    save('failed-card')
    time.sleep(.3)
    keys('n')
    finished()
    assert not (workspace / 'failed.txt').exists()
    check('a failed review asks the person, with a note that Auto review was unavailable, and never approves')

    start(thread, 'printf mine > mine.txt')
    until(lambda: len(pending()) == 1, 'a configured ask did not reach the person')
    assert pending()[0].get('review') is None, pending()
    until(lambda: 'printf mine' in card(capture()), 'the configured ask did not show')
    assert 'Auto review' not in card(capture()), card(capture())
    time.sleep(.3)
    keys('n')
    finished()
    assert not (workspace / 'mine.txt').exists()
    check('an ask the person configured for one command goes to them in Auto, unreviewed')

    start(thread, 'printf switched > switched.txt', 'stall')
    time.sleep(1)
    assert not pending(), 'a request under review is not shown'
    # Ctrl+Y from Auto goes on to Yolo, which approves; Review is one command away.
    command('/approvals review')
    until(lambda: len(pending()) == 1, 'switching to Review did not show the prompt', seconds=5)
    assert pending()[0].get('review') is None
    until(lambda: 'approvals: review' in capture() and 'printf switched' in card(capture()), 'the TUI did not show the prompt')
    time.sleep(.3)
    keys('n')
    finished()
    assert not (workspace / 'switched.txt').exists()
    keys('C-y')
    until(lambda: '  auto' in capture(), 'Ctrl+Y did not return to Auto')
    check('switching to Review mid-review ends the review and shows the prompt; nothing is approved')

    turn = start(thread, 'printf stopped > stopped.txt', 'stall')
    time.sleep(1)
    api('/v1/turns/' + turn['id'] + '/interrupt', {})
    finished()
    assert not pending() and not (workspace / 'stopped.txt').exists()
    check('Stop mid-review ends the review and the request with the turn')

    # haarness run --auto at a terminal: the reviewer's note, then the question.
    args = [binary, '--workspace', workspace, 'run', '--auto', '--title', 'Run asks',
            '!tool bash ' + json.dumps({'command': 'printf run-asked > run-asked.txt'}) + ' [review:ask]']
    run(['tmux', 'new-window', '-t', name, pane_command(env, args)])
    until(lambda: 'Allow it once? [y/N]' in capture(1), 'run --auto did not ask at the terminal', window=1)
    pane = capture(1)
    assert 'Auto review left it to a person (Test rule): Test verdict: ask.' in pane, pane
    save('run-terminal', window=1)
    keys('y', 'Enter', window=1)
    until(lambda: (workspace / 'run-asked.txt').exists(), 'the run did not go on after y', window=1)
    finished()
    check('run --auto at a terminal asks with the reviewer\'s note')

    # Without a terminal: each request that still needs a person is refused
    # with its reason, one line each (and one JSON record each), the agent
    # reads why in Haarness's words, and the run goes on to exit with 3.
    def headless_run(title, calls, verdict):
        return subprocess.run(
            [str(binary), '--workspace', str(workspace), 'run', '--auto', '--format', 'json', '--title', title,
             '!multi ' + json.dumps(calls) + f' [review:{verdict}]'],
            env=env, input='', text=True, capture_output=True, timeout=120, start_new_session=True)

    headless = headless_run('Run refuses', [
        {'name': 'bash', 'arguments': {'command': 'printf asked > run-asked-2.txt'}},
        {'name': 'bash', 'arguments': {'command': 'printf mine > run-mine.txt'}},
    ], 'ask')
    finished()
    assert headless.returncode == 3, (headless.returncode, headless.stderr)
    refused = sorted(line for line in headless.stderr.splitlines() if line.startswith('Refused: '))
    assert len(refused) == 2, headless.stderr
    assert any('A permission rule asks a person for it. Nobody can approve that in this run' in line for line in refused), refused
    assert any('Auto review left it to a person (Test rule): Test verdict: ask. Nobody can approve that in this run; allow it by adding' in line for line in refused), refused
    records = [json.loads(line) for line in headless.stdout.splitlines() if line.strip()]
    assert sorted(record['request'] for record in records if record['type'] == 'refused') == [
        'run a command (printf asked > run-asked-2.txt)', 'run a command (printf mine > run-mine.txt)'], records
    chat = next(record['threadID'] for record in records if record['type'] == 'refused')
    said = [item['data']['output'] for item in api('/v1/threads/' + chat + '/items')
            if item['data']['type'] == 'tool_result' and item['data'].get('is_error')]
    assert len(said) == 2 and all(output.startswith('Haarness refused this request for an unattended run: ') for output in said), said
    assert any('unattended run: Auto review left it to a person (Test rule): Test verdict: ask. Do not retry it' in output for output in said), said
    assert any('unattended run: A permission rule asks a person for it. Do not retry it' in output for output in said), said
    assert not any('The user rejected' in output for output in said), said
    assert not (workspace / 'run-asked-2.txt').exists() and not (workspace / 'run-mine.txt').exists()
    check('run --auto without a terminal refuses each request that needs a person, with its reason, goes on and exits 3')

    # Refusals between calls that succeed never stop the run.
    calls = []
    for index in range(6):
        calls.append({'name': 'bash', 'arguments': {'command': f'printf ok > ok-{index}.txt'}})
        calls.append({'name': 'bash', 'arguments': {'command': f'printf mine > mine-{index}.txt'}})
    interleaved = headless_run('Run goes on', calls, 'allow')
    finished()
    assert interleaved.returncode == 3, (interleaved.returncode, interleaved.stderr)
    assert 'Stopped:' not in interleaved.stderr, interleaved.stderr
    assert len([line for line in interleaved.stderr.splitlines() if line.startswith('Refused: ')]) == 6, interleaved.stderr
    assert all((workspace / f'ok-{index}.txt').exists() for index in range(6))
    assert not any((workspace / f'mine-{index}.txt').exists() for index in range(6))
    check('run --auto without a terminal goes on past refusals while other calls succeed')

    stuck = headless_run('Run stops', [
        {'name': 'bash', 'arguments': {'command': f'printf stuck > stuck-{index}.txt'}} for index in range(6)
    ], 'ask')
    finished()
    assert stuck.returncode == 1, (stuck.returncode, stuck.stderr)
    assert 'Stopped: 5 requests in a row needed a person, and nobody can answer in this run.' in stuck.stderr, stuck.stderr
    assert not any((workspace / f'stuck-{index}.txt').exists() for index in range(6))
    check('run --auto without a terminal stops after five refusals in a row')
    print(f'{len(checks)} Auto review scenarios passed.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', name], capture_output=True)
    subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=30)
    e2e_scratch.settle(root)
