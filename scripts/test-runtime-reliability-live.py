#!/usr/bin/env python3
"""Live-provider confirmation of allowed MCP use and read-only side questions."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request
import uuid
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
OUT = Path(os.environ.get('HAARNESS_LIVE_EVIDENCE', REPO / 'target/runtime-reliability/live'))
OUT.mkdir(parents=True, exist_ok=True)
key = os.environ.get('OPENAI_API_KEY')
if not key:
    auth = Path.home() / '.local/share/haarness/auth.json'
    credential = json.loads(auth.read_text()).get('openai', {}) if auth.exists() else {}
    if credential.get('type') == 'api':
        key = credential['key']
if not key:
    raise SystemExit('Live test requires an existing API credential')
model = os.environ.get('HAARNESS_LIVE_MODEL', 'gpt-5.4-mini')
with tempfile.TemporaryDirectory(prefix='haarness-live-port-') as temporary:
    root = Path(temporary).resolve()
    workspace = root / 'workspace'
    (workspace / '.haarness').mkdir(parents=True)
    nonce = 'harbor-' + uuid.uuid4().hex[:12]
    fixture = root / 'fixture.py'
    log = root / 'calls'
    fixture.write_text('''import json,sys
from pathlib import Path
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    method=request.get('method')
    if method=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
    elif method=='tools/list': result={'tools':[{'name':'ping','description':'Read the live verification marker','inputSchema':{'type':'object','properties':{},'additionalProperties':False}}]}
    elif method=='tools/call':
        with Path(sys.argv[2]).open('a') as output: output.write('called\\n')
        result={'content':[{'type':'text','text':sys.argv[1]}]}
    else: result={}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
''')
    config = f'[defaults]\nprovider="openai"\nmodel={json.dumps(model)}\n[permissions]\n"*"="deny"\n"mcp__fixture__*"="allow"\n[lazy_tools]\nmode="always"\n[lsp]\nenabled=false\n[mcp.fixture]\ntransport="stdio"\ncommand={json.dumps(sys.executable)}\nargs={json.dumps([str(fixture),nonce,str(log)])}\nenabled=true\n'
    (workspace / '.haarness/config.toml').write_text(config)
    subprocess.run(['git', 'init', '-q', str(workspace)], check=True)
    env = {**ISOLATED, 'PATH': os.environ['PATH'], 'HOME': str(root / 'home'), 'HAARNESS_HOME': str(root / 'home'), 'HAARNESS_DATA_DIR': str(root / 'data'), 'OPENAI_API_KEY': key, 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8'}
    args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
    connection = None
    try:
        connection = json.loads(subprocess.run(args + ['desktop'], input='{"action":"connect"}', env=env, text=True, capture_output=True, check=True, timeout=90).stdout)
        def api(path):
            request = urllib.request.Request(connection['url'] + '/v1' + path, headers={'Authorization': 'Bearer ' + connection['token']})
            with urllib.request.urlopen(request, timeout=20) as response: return json.load(response)
        prompt = 'Use mcp__fixture__ping to read the verification marker. Report the exact marker returned by the tool. Do not change files.'
        result = subprocess.run(args + ['run', '--provider', 'openai', '--model', model, '--no-thinking'], input=prompt, env=env, text=True, capture_output=True, timeout=180)
        (OUT / 'main.stdout').write_text(result.stdout)
        (OUT / 'main.stderr').write_text(result.stderr)
        assert result.returncode == 0 and nonce in result.stdout, 'live main reply missing marker; see evidence'
        assert log.read_text() == 'called\n', 'model did not make exactly one MCP call'
        thread = next(thread for thread in api('/threads?archived=true') if not thread.get('parentId'))['id']
        before = api(f'/threads/{thread}/items')
        assert any(i['data']['type'] == 'tool_result' and i['data'].get('output') == nonce for i in before)
        answer = subprocess.run(args + ['btw', thread, 'What exact verification marker did the previous tool return? Use only the existing conversation, without calling tools or changing files.'], env=env, text=True, capture_output=True, timeout=180)
        (OUT / 'side.stdout').write_text(answer.stdout)
        (OUT / 'side.stderr').write_text(answer.stderr)
        assert answer.returncode == 0 and nonce in answer.stdout, 'live side reply missing marker; see evidence'
        assert api(f'/threads/{thread}/items') == before, 'side question changed transcript'
        assert log.read_text() == 'called\n', 'side question called MCP'
        assert not api('/permissions') and not api('/turns/active')
        (OUT / 'items.json').write_text(json.dumps(before, indent=2))
        (OUT / 'results.json').write_text(json.dumps({'model': model, 'mainMcpCalls': 1, 'sideMcpCalls': 0, 'sideTranscriptUnchanged': True}, indent=2))
        print('PASS live model:', model, 'allowed MCP call and read-only side answer', flush=True)
    finally:
        if connection:
            subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
