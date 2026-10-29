#!/usr/bin/env python3
"""Real server, echo provider: searches meet the read rules outside a turn, and glob ends on link loops.

1. A workflow's own `grep` and `glob` leave out the files a `read` rule denies, as an agent's do, and
   say how many they left out.
2. The model's `glob` over a project with links back up its own tree (`a/up -> ..`, `a/here -> .`)
   and no match ends by itself, and still lists a linked folder of the project.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-workflow-search.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, shutil, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch
from e2e_env import clean_env

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-workflow-search-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; state = root/'state'
(state/'config').mkdir(parents=True); (state/'data').mkdir()
for name, text in {'private/key.txt': 'needle private', 'public/readme.md': 'needle public',
                   'a/note.txt': 'note', 'docs/guide.txt': 'guide'}.items():
    (workspace/name).parent.mkdir(parents=True, exist_ok=True); (workspace/name).write_text(text)
os.symlink('..', workspace/'a/up'); os.symlink('.', workspace/'a/here'); os.symlink('../docs', workspace/'a/docs')
(state/'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions.read]\n"private/*" = "deny"\n')
env = clean_env()
# A workflow's own calls run on Bun; keep the one this machine has on PATH.
bun = shutil.which('bun') or str(pathlib.Path.home()/'.bun/bin/bun')
env['PATH'] = f'{pathlib.Path(bun).parent}:{env.get("PATH", "/usr/bin:/bin")}'
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1')
url = None; token = None; checks = []


def run(args): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=120, env=env).stdout
def api(path, body=None):
    headers = {'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token}
    request = urllib.request.Request(url + '/v1' + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
    with urllib.request.urlopen(request, timeout=15) as reply:
        data = reply.read(); return json.loads(data) if data else None
def until(predicate, label, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if result := predicate(): return result
        time.sleep(.1)
    raise AssertionError(label)
def check(label): checks.append(label); print('PASS ' + label, flush=True)


try:
    subprocess.run(['git', 'init', '-q', str(workspace)], check=True)
    (workspace/'.haarness/workflows').mkdir(parents=True)
    (workspace/'.haarness/workflows/searcher.ts').write_text(
        'import { workflow } from "@haarness-ai/plugin"\n'
        'export default workflow({ name: "searcher", async run(args, ctx) {\n'
        '  const grep = await ctx.tool("grep", { pattern: "needle", path: "." })\n'
        '  const glob = await ctx.tool("glob", { pattern: "**/*.*" })\n'
        '  return { grep, glob }\n} })\n')
    output = run([str(binary), '--workspace', str(workspace), 'workflow', 'run', 'searcher'])
    assert 'public/readme.md:1:needle public' in output and 'public/readme.md' in output, output
    assert 'key.txt' not in output and 'needle private' not in output, output
    assert output.count('skipped 1 file a read rule denies') == 2, output
    check("a workflow's grep and glob leave out the file a read rule denies, and say so")

    # The workflow run started this project's server; a new one starts if it has gone.
    subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--daemon', '--port', '0'],
                   env=env, capture_output=True, timeout=120)
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    url, token = runtime['url'], runtime.get('token')
    thread = api('/threads', {'title': 'Glob loops', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']
    results = lambda: [item['data'] for item in api(f'/threads/{thread}/items') if item['data']['type'] == 'tool_result']
    outputs = []
    for pattern in ['**/*.nothing', 'a/**/*.txt']:
        before = len(results())
        started = time.monotonic()
        api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool glob ' + json.dumps({'pattern': pattern})})
        result = until(lambda: (values[before] if len(values := results()) > before else None), f'glob {pattern} did not end', 30)
        until(lambda: not api('/turns/active'), 'turn still active')
        assert not result['is_error'], result
        outputs.append((result['output'], time.monotonic() - started))
    assert outputs[0][0] == '', outputs
    assert outputs[1][0].splitlines() == ['a/docs/guide.txt', 'a/note.txt'], outputs
    check(f'glob over links back up the project ends ({outputs[0][1]:.1f} s with no match) and follows a linked folder')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} workflow search checks.', flush=True)
finally:
    subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
