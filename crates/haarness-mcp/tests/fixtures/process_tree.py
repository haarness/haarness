"""MCP fixture with normal and detached descendants; every child self-expires."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

log = Path(os.environ['TREE_LOG'])
if len(sys.argv) > 1:
    if sys.argv[1] == 'child':
        subprocess.Popen([sys.executable, __file__, 'detached'], start_new_session=True,
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    with log.open('a') as out:
        out.write(str(os.getpid()) + '\n')
    time.sleep(45)
    sys.exit(0)

log.write_text(str(os.getpid()) + '\n')
subprocess.Popen([sys.executable, __file__, 'child'], stdin=subprocess.DEVNULL,
                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
while len(log.read_text().splitlines()) < 3:
    time.sleep(.01)
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    if request.get('method') == 'initialize':
        time.sleep(float(os.environ.get('INIT_DELAY', '0')))
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}},
                  'serverInfo': {'name': 'process-tree', 'version': '1'}}
    elif request.get('method') == 'tools/list':
        result = {'tools': []}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
