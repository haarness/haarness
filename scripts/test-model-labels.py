#!/usr/bin/env python3
"""Real CLI and server, no network: Bedrock Claude models are listed by name whatever their region prefix
and version tag, and ids the name table does not know stay as written.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-model-labels.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, subprocess, tempfile, urllib.request
import e2e_scratch
from e2e_env import clean_env

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-model-labels-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
env = clean_env('AWS_')
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_NO_CONNECT_PROMPT='1')
EXPECTED = {
    'apac.anthropic.claude-sonnet-5-5-v1:0': 'Claude Sonnet 5.5',
    'jp.anthropic.claude-opus-4-6-v1': 'Claude Opus 4.6',
    'us-gov.anthropic.claude-haiku-4-5-20251001-v1:0': 'Claude Haiku 4.5 (2025-10-01)',
    # Not in the name table: shown as written, never guessed.
    'apac.anthropic.claude-sonnet-9-9-v1:0': 'apac.anthropic.claude-sonnet-9-9-v1:0',
}
url = None

def cli(*args):
    return subprocess.run([str(binary), '--workspace', str(workspace), *args], env=env, check=True,
                          text=True, capture_output=True, timeout=60).stdout

def names(listing):
    """`models --verbose` prints each model's id line followed by its JSON."""
    found, lines, index = {}, listing.splitlines(), 0
    while index < len(lines):
        if lines[index].startswith('bedrock-fixture/'):
            block = []
            index += 1
            while index < len(lines) and not lines[index].startswith(('bedrock-fixture/', 'echo/')):
                block.append(lines[index]); index += 1
            model = json.loads('\n'.join(block))
            found[model['id']] = model['displayName']
        else:
            index += 1
    return found

try:
    args = ['provider', 'add', 'bedrock-fixture', '--protocol', 'amazon-bedrock', '--no-auth',
            '--url', 'https://bedrock-runtime.ap-northeast-1.amazonaws.com']
    for model in EXPECTED: args += ['--model', model]
    cli(*args)
    found = names(cli('models', 'bedrock-fixture', '--verbose'))
    assert found == EXPECTED, found
    print('PASS haarness models lists Bedrock Claude ids by name: ' + json.dumps(found), flush=True)

    cli('serve', '--daemon', '--port', '0')
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    url = runtime['url']
    request = urllib.request.Request(url + '/v1/providers', headers={'Authorization': 'Bearer ' + runtime.get('token', '')})
    with urllib.request.urlopen(request, timeout=15) as reply:
        providers = json.loads(reply.read())
    served = {model['id']: model['displayName'] for provider in providers if provider['id'] == 'bedrock-fixture'
              for model in provider['models']}
    assert served == EXPECTED, served
    print('PASS the server gives clients (TUI, Desktop, iOS) the same names', flush=True)
    print(f'PASS 2 model label scenarios.', flush=True)
finally:
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
