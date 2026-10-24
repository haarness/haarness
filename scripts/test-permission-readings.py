#!/usr/bin/env python3
"""Real-binary proof of the October 24 permission-reading fixes, with the echo provider.

1. toml_order: `[permissions]` rules in `config.toml` apply in the order written, kept as TOML and
   once migrated to `haarness.json`: a deny written after a broader allow refuses.

Usage: python3 scripts/test-permission-readings.py   (HAARNESS_BIN picks the binary,
HAARNESS_E2E_ROOT a folder to keep the fixture in, HAARNESS_E2E_CASES a comma-separated subset of
toml_order). No model is called; nothing outside the fixture is touched.
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


def bash(env, folder, command, timeout=120):
    """`haarness run` of one bash call: what the run printed."""
    line = '!tool bash ' + json.dumps({'command': command})
    result = subprocess.run([str(BINARY), '--workspace', str(folder), '--trust-project', 'run', line],
                            env=env, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    return result.stdout + result.stderr


def stop(env, folder):
    subprocess.run([str(BINARY), '--workspace', str(folder), 'serve', '--stop'], env=env,
                   capture_output=True, timeout=60, stdin=subprocess.DEVNULL)


def passed(label):
    checks.append(label)
    print('PASS', label, flush=True)


def case_toml_order():
    """A deny after a broader allow in `config.toml` refuses, kept as TOML or migrated."""
    rules = '[permissions.bash]\n"echo hello*" = "allow"\n"echo hello --force*" = "deny"\n'
    for name, extra in [('toml-kept', '[compaction]\nthreshold = 0.9\n'), ('toml-migrated', '')]:
        config, env = environment(name)
        (config / 'config.toml').write_text(rules + extra)
        folder = project(name)
        try:
            output = bash(env, folder, 'echo hello --force')
            assert "is denied by permission rule 'bash:echo hello --force*'" in output, output
            output = bash(env, folder, 'echo hello world')
            assert 'hello world' in output and 'denied' not in output, output
        finally:
            stop(env, folder)
        if not extra:
            migrated = json.loads((config / 'haarness.json').read_text())
            assert list(migrated['permission']['bash']) == ['echo hello*', 'echo hello --force*'], migrated
        passed(f'{name}: a deny written after a broader allow refuses')


CASES = {'toml_order': case_toml_order}

if __name__ == '__main__':
    chosen = [name for name in os.environ.get('HAARNESS_E2E_CASES', ','.join(CASES)).split(',') if name]
    for name in chosen:
        CASES[name]()
    print(f'{len(checks)} checks passed')
    sys.exit(0)
