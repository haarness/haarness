#!/usr/bin/env python3
"""Real-binary proof of the October 16 permission, schedule and hook fixes, with the echo provider.

1. A `read` rule written relative to the project refuses the file's absolute path, and an
   absolute `edit` rule a relative write.
2. `haarness run` without a terminal stops on an approval and names the allow rule to add.
3. A scheduled run never waits: a workflow it starts runs its `ctx.shell` call that an `ask`
   rule covers, and a Cloud action that asks every time is refused at once.
4. A workflow's agents get no SessionStart hook; the chats people start do.
5. A plugin's workflow asked for by its bare name names the invocation to use.
6. (October 18) A list through a link meets the rules for the folder it lists, and grep and glob
   leave out a file a read rule denies; an edit of the
   project's own haarness.json asks a person even under `"edit": "allow"`, `--yolo` and a schedule,
   unless a rule names the file; `$HOME/...` in a shell command asks for the home folder; code
   frontmatter (`---js`) is skipped while `---json` keeps an agent's limits.

Usage: python3 scripts/test-permission-boundaries.py   (HAARNESS_BIN picks the binary,
HAARNESS_E2E_ROOT a folder to keep the fixture in, HAARNESS_E2E_CASES a comma-separated subset
of paths, headless, schedules, session_start, plugin_workflow, config_files, home_variable and
frontmatter). No model is called.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-permission-boundaries-')
root = Path(keep or holder.name).resolve()
root.mkdir(parents=True, exist_ok=True)
home, data = root / 'home', root / 'data'
# Native workflows run on Bun; keep the one this machine has on PATH.
bun = shutil.which('bun') or str(Path.home() / '.bun/bin/bun')
env = {'PATH': f'{Path(bun).parent}:/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
       'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_SCHEDULE_SKIP_INSTALL': '1'}
projects = []
checks = []


def project(name, config):
    folder = root / name
    (folder / '.haarness').mkdir(parents=True, exist_ok=True)
    (folder / 'haarness.json').write_text(json.dumps({'model': 'echo/echo-1', **config}))
    subprocess.run(['git', 'init', '-q', str(folder)], check=True)
    projects.append(folder)
    return folder


def haarness(folder, *args, check=True, timeout=120):
    result = subprocess.run([str(BINARY), '--workspace', str(folder), '--trust-project', *args],
                            env=env, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    if check:
        assert result.returncode == 0, f'{args}: {result.stdout}\n{result.stderr}'
    return result


def until(check, label, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.2)
    raise AssertionError(label)


def passed(label):
    checks.append(label)
    print('PASS', label, flush=True)


def connect(folder):
    """The folder's server, as Desktop reaches it."""
    result = subprocess.run([str(BINARY), '--workspace', str(folder), '--trust-project', 'desktop'],
                            env=env, capture_output=True, text=True, timeout=120, input='{"action":"connect"}')
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def api(connection, path):
    request = urllib.request.Request(connection['url'] + '/v1' + path,
                                     headers={'Authorization': 'Bearer ' + connection['token']})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read())


def case_paths():
    """Path rules name a project file by either spelling."""
    paths = project('paths', {'permission': {
        'read': {'secrets/*': 'deny'},
        'edit': {str(root / 'paths' / 'vendor') + '/*': 'deny', 'generated/*': 'deny'},
        'list': {'build': 'deny'}}})
    (paths / 'secrets').mkdir()
    (paths / 'secrets' / 'key.txt').write_text('TOPSECRET-VALUE\n')
    absolute = json.dumps({'path': str(paths / 'secrets' / 'key.txt')})
    output = haarness(paths, 'run', f'!tool read {absolute}').stdout
    assert 'TOPSECRET-VALUE' not in output, output
    assert "is denied by permission rule 'read:secrets/*'" in output, output
    write = json.dumps({'path': 'vendor/lib.rs', 'content': 'changed'})
    output = haarness(paths, 'run', f'!tool write {write}').stdout
    assert f"denied by permission rule 'edit:{paths}/vendor/*'" in output, output
    assert not (paths / 'vendor' / 'lib.rs').exists()
    passed('a relative read deny refuses an absolute path, and an absolute edit deny a relative write')
    # `./`, a trailing slash, `..` inside a patch and a patch's move target.
    output = haarness(paths, 'run', '!tool read {"path":"./secrets/key.txt"}').stdout
    assert 'TOPSECRET-VALUE' not in output and "denied by permission rule 'read:secrets/*'" in output, output
    output = haarness(paths, 'run', '!tool write {"path":"./generated/a.rs","content":"x"}').stdout
    assert "denied by permission rule 'edit:generated/*'" in output, output
    (paths / 'build').mkdir()
    output = haarness(paths, 'run', '!tool list {"path":"build/"}').stdout
    assert "denied by permission rule 'list:build'" in output, output
    (paths / 'alias').symlink_to('build')
    output = haarness(paths, 'run', '!tool list {"path":"alias"}').stdout
    assert "denied by permission rule 'list:build'" in output, output
    # Searches leave out what a read rule denies, and say how many.
    output = haarness(paths, 'run', '!tool grep {"pattern":"TOPSECRET"}').stdout
    assert 'TOPSECRET-VALUE' not in output and '(skipped 1 file a read rule denies)' in output, output
    output = haarness(paths, 'run', '!tool glob {"pattern":"**/*.txt"}').stdout
    assert 'secrets/key.txt' not in output and '(skipped 1 file a read rule denies)' in output, output
    (paths / 'src').mkdir()
    (paths / 'src' / 'old.rs').write_text('a\n')
    for patch in ('*** Begin Patch\n*** Add File: src/../generated/b.rs\n+b\n*** End Patch',
                  '*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: generated/moved.rs\n@@\n-a\n+moved\n*** End Patch'):
        output = haarness(paths, 'run', '!tool apply_patch ' + json.dumps({'input': patch})).stdout
        assert "denied by permission rule 'edit:generated/*'" in output, output
    assert not (paths / 'generated').exists() and (paths / 'src' / 'old.rs').read_text() == 'a\n'
    passed('./, a trailing slash, .. in a patch and a patch\'s Move to meet the same rules')


def case_headless():
    """A run nobody can answer names the allow rule."""
    headless = project('headless', {'permission': {'bash': 'ask'}})
    result = haarness(headless, 'run', '!tool bash {"command":"git status"}', check=False)
    assert result.returncode != 0, result.stdout
    assert 'Stopped: the agent asked to run a command (git status), and nobody can approve that in this run.' in result.stderr, result.stderr
    assert 'Allow it by adding {"permission":{"bash":{"git status' in result.stderr, result.stderr
    assert 'to haarness.json in the project or ~/.config/haarness/haarness.json, or run again with --yolo' in result.stderr, result.stderr
    passed('a refused headless run names the allow rule and where it goes')


def case_schedules():
    """Scheduled runs never wait for a person."""
    scheduled = project('scheduled', {'permission': {'bash': 'ask'}})
    (scheduled / '.haarness' / 'workflows').mkdir(parents=True)
    (scheduled / '.haarness' / 'workflows' / 'nightly.ts').write_text(
        'import { workflow } from "@haarness-ai/plugin"\n'
        'export default workflow({ name: "nightly", async run(args, ctx) {\n'
        '  await ctx.shell("echo SCHEDULED_SHELL_RAN > shell-marker.txt")\n'
        '  return "done"\n} })\n')
    start = json.dumps({'action': 'start', 'name': 'nightly'})
    haarness(scheduled, 'schedule', 'add', '--name', 'nightly-workflow', '--every', '1d',
             '--prompt', f'!tool workflow {start}')
    nightly = json.loads(haarness(scheduled, 'schedule', 'run', 'nightly-workflow', '--json').stdout)
    assert nightly['status'] == 'success', nightly
    until(lambda: (scheduled / 'shell-marker.txt').exists(), "the scheduled workflow's shell call waited for a person")
    delete = json.dumps({'action': 'delete', 'id': '01900000-0000-7000-8000-000000000000'})
    haarness(scheduled, 'schedule', 'add', '--name', 'cloud-delete', '--every', '1d',
             '--prompt', f'!tool cloud_agent_manage {delete}')
    run = json.loads(haarness(scheduled, 'schedule', 'run', 'cloud-delete', '--json', timeout=60).stdout)
    assert run['status'] == 'success', run
    assert "needs the user's approval every time, and this scheduled run cannot ask" in run['response'], run
    assert api(connect(scheduled), '/permissions') == [], 'a scheduled run left an approval waiting'
    passed("a scheduled run's workflow shell call runs, and a Cloud delete is refused at once")
    # Someone forks last night's run and works there: they are asked, for the
    # workflow their turn starts and for what always asks.
    (scheduled / '.haarness' / 'workflows' / 'manual.ts').write_text(
        'import { workflow } from "@haarness-ai/plugin"\n'
        'export default workflow({ name: "manual", async run(args, ctx) {\n'
        '  await ctx.shell("echo FORK_SHELL_RAN > fork-marker.txt")\n'
        '  return "done"\n} })\n')
    manual = json.dumps({'action': 'start', 'name': 'manual'})
    haarness(scheduled, '--session', nightly['threadId'], 'run', '--fork', f'!tool workflow {manual}')
    connection = connect(scheduled)
    waiting = until(lambda: [pending for pending in api(connection, '/permissions')
                             if 'FORK_SHELL_RAN' in pending['operation']], 'the fork\'s workflow did not ask')
    time.sleep(1)
    assert not (scheduled / 'fork-marker.txt').exists(), "the fork's workflow ran without asking"
    result = haarness(scheduled, '--session', nightly['threadId'], 'run', '--fork',
                      f'!tool cloud_agent_manage {delete}', check=False)
    assert result.returncode != 0, result.stdout
    assert 'which always needs an answer, and nobody can give one in this run' in result.stderr, result.stderr
    assert 'this scheduled run cannot ask' not in result.stdout + result.stderr, result.stdout
    passed('a fork of a scheduled run asks a person, for its workflow and for a Cloud delete')


def case_session_start():
    """SessionStart runs for chats people start, not for a workflow's agents."""
    starts = root / 'session-starts.jsonl'
    hooked = project('hooked', {'hooks': {'SessionStart': [{'hooks': [
        {'type': 'command', 'command': f"cat >> '{starts}'; echo >> '{starts}'"}]}]}})
    (hooked / '.haarness' / 'workflows').mkdir(parents=True)
    (hooked / '.haarness' / 'workflows' / 'fanout.ts').write_text(
        'import { workflow } from "@haarness-ai/plugin"\n'
        'export default workflow({ name: "fanout", async run(args, ctx) {\n'
        '  const replies = await ctx.parallel([() => ctx.agent({ prompt: "first look" }),\n'
        '                                      () => ctx.agent({ prompt: "second look" })])\n'
        '  return replies.map(reply => reply?.text).join(" | ")\n} })\n')
    haarness(hooked, 'workflow', 'run', 'fanout', timeout=180)
    connection = connect(hooked)
    sessions = [json.loads(line)['session_id'] for line in starts.read_text().splitlines() if line.strip()]
    assert sessions, 'no SessionStart ran'
    parents = [api(connection, f'/threads/{session}').get('parentId') for session in sessions]
    assert all(parent is None for parent in parents), f'SessionStart ran for a chat started inside another: {parents}'
    passed(f"SessionStart ran for {len(sessions)} chat(s) people started and none of the workflow's agents")


def case_plugin_workflow():
    """A plugin's workflow by its bare name."""
    plugin = project('plugin', {})
    bundle = plugin / '.haarness' / 'plugins' / 'shipping'
    (bundle / '.claude-plugin').mkdir(parents=True)
    (bundle / '.claude-plugin' / 'plugin.json').write_text(json.dumps({'name': 'shipping', 'workflows': ['./flows/audit.ts']}))
    (bundle / 'flows').mkdir()
    (bundle / 'flows' / 'audit.ts').write_text('export default workflow({ name: "audit", run() { return "ok" } })\n')
    listed = haarness(plugin, 'workflow', 'list').stdout
    assert 'shipping:audit' in listed, listed
    result = haarness(plugin, 'workflow', 'source', 'audit', check=False)
    assert result.returncode != 0, result.stdout
    assert "a plugin's workflow is invoked as plugin:name: shipping:audit" in result.stderr, result.stderr
    assert 'run() { return "ok" }' in haarness(plugin, 'workflow', 'source', 'shipping:audit').stdout
    passed('a bare plugin workflow name points to shipping:audit')


def case_config_files():
    """The project's own configuration asks a person before the agent changes it."""
    guarded = project('config-files', {'permission': {'edit': 'allow', 'bash': 'ask'}})
    original = (guarded / 'haarness.json').read_text()
    widen = json.dumps({'path': 'haarness.json', 'content': json.dumps({'permission': {'bash': 'allow'}})})
    for flags in ([], ['--yolo']):
        result = haarness(guarded, 'run', *flags, f'!tool write {widen}', check=False)
        assert result.returncode != 0, result.stdout
        assert 'which always needs an answer, and nobody can give one in this run' in result.stderr, result.stderr
        assert (guarded / 'haarness.json').read_text() == original, 'the agent rewrote haarness.json'
    (guarded / 'notes.json').symlink_to('haarness.json')
    linked = json.dumps({'path': 'notes.json', 'content': '{}'})
    result = haarness(guarded, 'run', f'!tool write {linked}', check=False)
    assert result.returncode != 0 and (guarded / 'haarness.json').read_text() == original, result.stdout
    # Ordinary files still follow `"edit": "allow"`.
    haarness(guarded, 'run', '!tool write {"path":"src/ok.txt","content":"ok"}')
    assert (guarded / 'src' / 'ok.txt').read_text() == 'ok'
    passed('an edit of haarness.json, also through a link and with --yolo, stops for a person under "edit": "allow"')
    haarness(guarded, 'schedule', 'add', '--name', 'widen', '--every', '1d', '--prompt', f'!tool write {widen}')
    run = json.loads(haarness(guarded, 'schedule', 'run', 'widen', '--json', timeout=60).stdout)
    assert run['status'] == 'success', run
    assert "'edit:haarness.json' needs the user's approval every time, and this scheduled run cannot ask" in run['response'], run
    assert (guarded / 'haarness.json').read_text() == original
    assert api(connect(guarded), '/permissions') == [], 'a scheduled run left an approval waiting'
    passed('a scheduled run is refused the edit at once and leaves nothing waiting')
    named = project('config-named', {'permission': {'edit': {'*': 'allow', 'pkg/haarness.json': 'allow'}}})
    nested = json.dumps({'path': 'pkg/haarness.json', 'content': '{"model":"echo/echo-1"}'})
    haarness(named, 'run', f'!tool write {nested}')
    assert (named / 'pkg' / 'haarness.json').exists()
    passed('a rule naming the file lets the edit run')


def case_home_variable():
    """A shell command that writes under $HOME asks for that folder."""
    home_project = project('home-variable', {'permission': {'bash': 'allow'}})
    target = home / 'outside-home-variable.txt'
    for command in ['echo x > $HOME/outside-home-variable.txt', 'touch "${HOME}/outside-home-variable.txt"']:
        result = haarness(home_project, 'run', '!tool bash ' + json.dumps({'command': command}), check=False)
        assert result.returncode != 0, result.stdout
        assert 'outside the project' in result.stderr or 'external' in result.stderr, result.stderr
        assert str(home) in result.stderr, result.stderr
        assert not target.exists(), f'{command} wrote outside the project without asking'
    passed('$HOME and ${HOME} in a shell command ask for the home folder')


def case_frontmatter():
    """Frontmatter is YAML or JSON, never code."""
    front = project('frontmatter', {})
    agents = front / '.haarness' / 'agents'
    agents.mkdir(parents=True)
    (agents / 'limited.md').write_text('---json\n{"description": "Limited", "permission": {"bash": "deny"}}\n---\nReview only.\n')
    (agents / 'scripted.md').write_text('---js\nmodule.exports = { description: "Scripted" }\n---\nDo anything.\n')
    listed = json.loads(haarness(front, 'agents', '--all', '--json').stdout)
    names = {agent.get('id') or agent.get('name') for agent in listed}
    assert 'limited' in names, listed
    assert 'scripted' not in names, listed
    output = haarness(front, 'run', '--agent', 'limited', '!tool bash {"command":"echo should-not-run"}', check=False)
    shown = output.stdout + output.stderr
    assert 'tool bash is disabled' in shown or "is denied by permission rule 'bash:*'" in shown, shown
    logs = '\n'.join(path.read_text(errors='replace') for path in root.rglob('*.log*') if path.is_file())
    assert 'skipped agent' in logs and 'JavaScript frontmatter is not supported' in logs, 'no skip notice in the log'
    passed('a ---json agent keeps its bash deny, and a ---js agent is skipped with a notice')


CASES = {name: globals()['case_' + name] for name in ['paths', 'headless', 'schedules', 'session_start', 'plugin_workflow',
                                                       'config_files', 'home_variable', 'frontmatter']}
selected = [name for name in os.environ.get('HAARNESS_E2E_CASES', ','.join(CASES)).split(',') if name]
try:
    for name in selected:
        CASES[name]()
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    for folder in projects:
        subprocess.run([str(BINARY), '--workspace', str(folder), 'serve', '--stop'], env=env,
                       capture_output=True, timeout=60)
    if holder:
        holder.cleanup()
