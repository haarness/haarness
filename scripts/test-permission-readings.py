#!/usr/bin/env python3
r"""Real-binary proof of the October 24 permission-reading fixes, with the echo provider.

1. toml_order: `[permissions]` rules in `config.toml` apply in the order written, a `"*"` catch-all
   first, kept as TOML and once migrated to `haarness.json`: a deny written after a broader allow
   refuses, and a catch-all written last never overrides a deny before it.
2. cd_link: a `cd` through a link that the shell takes out of the project by name
   (`cd a/l/../../..` with `a/l` leading deeper in) asks, so a headless run stops before it writes.
3. resource_alias: a deny written in `opencode.json` under upstream OpenCode's name for the resource
   read tool refuses `mcp_resource_read`.
4. backslash: a line that starts with a backslash is a command of its own (an `rm *` deny refuses
   `true` followed by `\rm -rf victim`, and an allowed `echo *` under `"*": "ask"` does not open
   `echo hi` followed by one), and a line continuation inside a word is joined (`r\` + `m -rf`).
5. backquotes: the commands in backquotes of an unquoted here-document meet the rules (an `rm *`
   deny refuses `cat > n.md <<EOF` holding `rm -rf victim` in backquotes), while one under an
   allowed `cat *` with `"*": "ask"` runs without a question, as before.
6. dev: a redirect into `/dev/shm` asks for the folder like any path outside the project, while
   `/dev/null`, `/dev/stderr` and the other stream devices ask nothing.
7. action_case: `"Deny"` in haarness.json denies, and a value that is no action is ignored with one
   warning in the log that names the file and the key.

Usage: python3 scripts/test-permission-readings.py   (HAARNESS_BIN picks the binary,
HAARNESS_E2E_ROOT a folder to keep the fixture in, HAARNESS_E2E_CASES a comma-separated subset of
toml_order, cd_link, resource_alias, backslash, backquotes, dev, action_case). No model is called; nothing outside the fixture is
touched.
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-permission-readings-')
root = Path(keep or holder.name).resolve()
root.mkdir(parents=True, exist_ok=True)
checks = []
# Chats start in Auto, where the reviewer answers an ask: a case about a person's prompt asks for Review.
REVIEW = {'defaults': {'approval_mode': 'review'}}


def environment(name):
    """An isolated home and data folder for one case, and the global configuration folder."""
    home, data = root / name / 'home', root / name / 'data'
    home.mkdir(parents=True, exist_ok=True)
    (data / 'config').mkdir(parents=True, exist_ok=True)
    return data / 'config', {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
                  'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'),
                  'LANG': 'en_US.UTF-8', 'HAARNESS_TEST_PROVIDER': '1'}


def project(name, config=None):
    folder = root / name / 'project'
    folder.mkdir(parents=True, exist_ok=True)
    (folder / 'haarness.json').write_text(json.dumps({'model': 'echo/echo-1', **(config or {})}))
    subprocess.run(['git', 'init', '-q', str(folder)], check=True)
    return folder


def bash(env, folder, command, timeout=120, code=None):
    """`haarness run` of one bash call: what the run printed. `code` checks how it exited."""
    return call(env, folder, 'bash', {'command': command}, timeout, code)


def call(env, folder, name, arguments, timeout=120, code=None):
    """`haarness run` of one tool call: what the run printed. `code` checks how it exited."""
    line = f'!tool {name} ' + json.dumps(arguments)
    result = subprocess.run([str(BINARY), '--workspace', str(folder), '--trust-project', 'run', line],
                            env=env, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    output = result.stdout + result.stderr
    if code is not None:
        assert (result.returncode == 0) == (code == 0), f'exit {result.returncode}: {output}'
    return output


def stop(env, folder):
    subprocess.run([str(BINARY), '--workspace', str(folder), 'serve', '--stop'], env=env,
                   capture_output=True, timeout=60, stdin=subprocess.DEVNULL)


def passed(label):
    checks.append(label)
    print('PASS', label, flush=True)


def case_toml_order():
    """`config.toml` rules apply as written, a `"*"` catch-all first; kept as TOML or migrated."""
    rules = ('[permissions]\nbash = { "echo hello*" = "allow", "echo hello --force*" = "deny", "*" = "allow",'
             ' "echo secret*" = "deny" }\n"*" = "allow"\nedit = "ask"\n')
    for name, extra in [('toml-kept', '[compaction]\nthreshold = 0.9\n'), ('toml-migrated', '')]:
        config, env = environment(name)
        (config / 'config.toml').write_text(rules + extra)
        folder = project(name)
        try:
            output = bash(env, folder, 'echo hello --force')
            assert "is denied by permission rule 'bash:echo hello --force*'" in output, output
            # The catch-all written after it never overrides the deny before it.
            output = bash(env, folder, 'echo secret x')
            assert "is denied by permission rule 'bash:echo secret*'" in output, output
            output = bash(env, folder, 'echo hello world')
            assert 'hello world' in output and 'denied' not in output, output
        finally:
            stop(env, folder)
        if not extra:
            migrated = json.loads((config / 'haarness.json').read_text())['permission']
            assert list(migrated) == ['*', 'edit', 'bash'], migrated
            assert list(migrated['bash']) == ['*', 'echo hello*', 'echo hello --force*', 'echo secret*'], migrated
        passed(f'{name}: a deny written after a broader allow refuses, and a catch-all written last reads first')


def case_cd_link():
    """A cd the shell takes out of the project by name asks before the line runs."""
    _, env = environment('cd-link')
    folder = project('cd-link', REVIEW)
    (folder / 'deep' / 'er' / 'x').mkdir(parents=True)
    (folder / 'a').mkdir()
    (folder / 'a' / 'l').symlink_to('../deep/er/x')
    escaped = folder.parent / 'escaped.txt'
    try:
        output = bash(env, folder, 'cd a/l/../../.. && touch escaped.txt', code=1)
        assert 'Stopped:' in output and str(folder.parent) in output, output
        assert not escaped.exists(), 'the line ran outside the project without asking'
        output = bash(env, folder, 'cd a/l/.. && pwd', code=0)
        assert str(folder / 'a') in output, output
    finally:
        stop(env, folder)
    passed('a cd out of the project through a link asks; one that stays inside runs')


def case_resource_alias():
    """`opencode_read_mcp_resource` in opencode.json governs `mcp_resource_read`."""
    _, env = environment('resource-alias')
    fixture = REPO / 'crates/haarness-mcp/tests/fixtures/resources.py'
    folder = project('resource-alias', {'mcp': {'docs': {'command': sys.executable, 'args': [str(fixture)]}}})
    (folder / 'opencode.json').write_text(json.dumps({'permission': {'opencode_read_mcp_resource': {'*': 'deny'}}}))
    try:
        output = call(env, folder, 'mcp_resource_read', {'server': 'docs', 'uri': 'docs://setup'})
        assert 'Run the fixture' not in output, output
        assert 'tool mcp_resource_read is disabled' in output, output
        output = call(env, folder, 'mcp_resource_list', {'server': 'docs'}, code=0)
        assert 'docs://setup' in output, output
    finally:
        stop(env, folder)
    passed('a deny under the upstream resource read name refuses the read; the listing still runs')


def case_backslash():
    """Backslash lines meet the rules for the commands the shell runs."""
    _, env = environment('backslash')
    denied = project('backslash', {'permission': {'bash': {'rm *': 'deny'}}})
    for command in ['true\n\\rm -rf victim', 'true\n\\\nrm -rf victim', 'r\\\nm -rf victim',
                    "bash <<'EOF'\n\\rm -rf victim\nEOF", 'echo "<<rm"\n\\rm -rf victim',
                    'true # <<rm\n\\rm -rf victim', 'true \\\r\nrm -rf victim', 'rm\\\n -rf victim']:
        (denied / 'victim').mkdir(exist_ok=True)
        try:
            output = bash(env, denied, command)
        finally:
            stop(env, denied)
        assert "is denied by permission rule 'bash:rm *'" in output, (command, output)
        assert (denied / 'victim').is_dir(), f'{command!r} deleted victim'
    passed('an rm deny refuses rm on a backslash line (also after a `<<` in a string or a comment), after a lone '
           'backslash, after a backslash and CR LF, joined across a continuation, and on the first line of a '
           'here-document fed to bash')
    asked = project('backslash-ask', {**REVIEW, 'permission': {'bash': {'*': 'ask', 'echo *': 'allow'}}})
    (asked / 'victim').mkdir()
    try:
        output = bash(env, asked, 'echo hi\n\\rm -rf victim', code=1)
        assert 'Stopped:' in output and 'rm -rf victim' in output, output
        assert (asked / 'victim').is_dir(), 'the allowed echo opened the rm after it'
        output = bash(env, asked, 'echo hi\necho there', code=0)
        assert 'there' in output, output
    finally:
        stop(env, asked)
    passed('an allowed echo does not open a backslash line after it; plain echo lines still run')


def case_backquotes():
    """Backquotes in an unquoted here-document meet the rules for what they run."""
    _, env = environment('backquotes')
    denied = project('backquotes', {'permission': {'bash': {'rm *': 'deny'}}})
    (denied / 'victim').mkdir()
    try:
        for command in ['cat > n.md <<EOF\n`rm -rf victim`\nEOF',
                        'while read -r l; do echo "$l"; done <<EOF\n`rm -rf victim`\nEOF',
                        'cat > n.md <<EOF\n${X:-`rm -rf victim`}\nEOF']:
            output = bash(env, denied, command)
            assert "is denied by permission rule 'bash:rm *'" in output, (command, output)
            assert (denied / 'victim').is_dir(), f'the backquoted rm ran: {command!r}'
        output = bash(env, denied, "cat > q.md <<'EOF'\n`rm -rf victim`\nEOF", code=0)
        assert (denied / 'q.md').read_text() == '`rm -rf victim`\n', output
    finally:
        stop(env, denied)
    asked = project('backquotes-ask', {**REVIEW, 'permission': {'bash': {'*': 'ask', 'cat *': 'allow'}}})
    try:
        bash(env, asked, 'cat > notes.md <<EOF\nRun `echo hi` first.\nEOF', code=0)
        assert (asked / 'notes.md').read_text() == 'Run hi first.\n'
    finally:
        stop(env, asked)
    passed('an rm deny refuses rm in backquotes of an unquoted here-document (also of a loop, and in a ${…}); '
           'a quoted one and an allowed cat with backquotes run as before')


def case_dev():
    """Only stream devices skip the outside-folder check of a redirect."""
    _, env = environment('dev')
    folder = project('dev', REVIEW)
    try:
        output = bash(env, folder, 'echo x > /dev/shm/haarness-e2e-probe', code=1)
        assert 'Stopped:' in output and '/dev/shm' in output, output
        output = bash(env, folder, 'echo quiet > /dev/null && echo loud > /dev/stderr && echo done', code=0)
        assert 'done' in output and 'Stopped' not in output, output
    finally:
        stop(env, folder)
    passed('a redirect into /dev/shm asks; /dev/null and /dev/stderr do not')


def case_action_case():
    """A permission action in any letter case applies; another value is named in the log."""
    _, env = environment('action-case')
    folder = project('action-case', {'permission': {'bash': {'echo hello*': 'Deny', 'echo bye*': 'dney'}}})
    try:
        output = bash(env, folder, 'echo hello there')
        assert "is denied by permission rule 'bash:echo hello*'" in output, output
        output = bash(env, folder, 'echo bye now', code=0)
        assert 'bye now' in output, output
    finally:
        stop(env, folder)
    logs = '\n'.join(path.read_text() for path in (root / 'action-case' / 'data').rglob('haarness.log*'))
    warning = (f'config: ignored "permission.bash.echo bye*" in {folder / "haarness.json"}: '
               '"dney" is not allow, ask or deny')
    assert logs.count(warning) == 1, logs[-2000:]
    passed('"Deny" denies; "dney" is ignored and the log names the file and the key')


CASES = {'toml_order': case_toml_order, 'cd_link': case_cd_link, 'resource_alias': case_resource_alias,
         'backslash': case_backslash, 'backquotes': case_backquotes, 'dev': case_dev,
         'action_case': case_action_case}

if __name__ == '__main__':
    chosen = [name for name in os.environ.get('HAARNESS_E2E_CASES', ','.join(CASES)).split(',') if name]
    for name in chosen:
        CASES[name]()
    print(f'{len(checks)} checks passed')
    sys.exit(0)
