#!/usr/bin/env python3
"""Real servers: requests a web page starts are refused, terminal clients are not.

An open loopback server (plain `haarness serve`) and an `--anonymous` server on
0.0.0.0 need no credentials, so only the Host/Origin/Sec-Fetch-Site checks stand
between them and any web page: an image or link (no Origin, `Sec-Fetch-Site:
cross-site`), and a page whose DNS name was re-pointed at the server (Host and
Origin both name the attacker's site). The CLI attaches to both afterwards.
"""
import http.client, json, os, pathlib, subprocess, tempfile, time
import e2e_scratch

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-cross-site-')).resolve()


def run(args, env, check=True):
    result = subprocess.run(args, text=True, capture_output=True, timeout=60, env=env)
    if check and result.returncode != 0:
        raise AssertionError(f'{args} failed: {result.stderr}')
    return result


def request(port, path='/v1/info', method='GET', host=None, **headers):
    """Status of one request to 127.0.0.1:port, with a Host of our choosing."""
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=10)
    connection.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
    connection.putheader('Host', host or f'127.0.0.1:{port}')
    for name, value in headers.items():
        connection.putheader(name.replace('_', '-'), value)
    connection.putheader('Content-Length', '0')
    connection.endheaders()
    response = connection.getresponse()
    response.read()
    connection.close()
    return response.status


servers = []
try:
    for name, extra in [('loopback', ['--host', '127.0.0.1']), ('network', ['--host', '0.0.0.0', '--anonymous'])]:
        state = root / name
        workspace = state / 'project'
        workspace.mkdir(parents=True)
        env = {**os.environ, 'HAARNESS_DATA_DIR': str(state / 'state'), 'HAARNESS_HOME': str(state / 'home'),
               'HOME': str(state / 'home'), 'HAARNESS_SERVER_TOKEN': '', 'HAARNESS_SERVER_PASSWORD': '',
               'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_DEFAULT_PROVIDER': 'echo', 'HAARNESS_DEFAULT_MODEL': 'echo-1'}
        run([str(binary), '--workspace', str(workspace), 'serve', '--daemon', '--port', '0', *extra], env)
        servers.append((workspace, env))
        registry = json.loads((state / 'state/data/workspaces.json').read_text())
        identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
        url = json.loads((state / 'state/cache/workspaces' / identity / 'runtime.json').read_text())['url']
        port = int(url.rsplit(':', 1)[1].rstrip('/'))

        # Terminal clients send neither Origin nor Sec-Fetch-Site.
        assert request(port) == 200
        assert request(port, Sec_Fetch_Site='none') == 200, 'the user typing the address'
        assert request(port, Sec_Fetch_Site='same-origin') == 200
        # <img src=…>, a link, or a no-cors fetch from a web page.
        assert request(port, Sec_Fetch_Site='cross-site') == 403
        assert request(port, Sec_Fetch_Site='same-site') == 403
        assert request(port, '/v1/threads', 'POST', Sec_Fetch_Site='cross-site') == 403
        assert request(port, '/v1/threads', 'POST', Origin='https://unrelated.example') == 403
        assert request(port, Origin=f'http://127.0.0.1:{port}') == 200
        # Public diagnostics stay readable.
        assert request(port, '/healthz', Sec_Fetch_Site='cross-site') == 200

        if name == 'network':
            rebound = f'attacker.example:{port}'
            # DNS rebinding: the page's own name now resolves to this server.
            assert request(port, host=rebound, Origin=f'http://{rebound}') == 403
            assert request(port, '/v1/threads', 'POST', host=rebound, Origin=f'http://{rebound}') == 403
            assert request(port, host=rebound, Sec_Fetch_Site='same-origin') == 403
            # Same address, another port: another web site.
            assert request(port, Origin=f'http://127.0.0.1:{port + 1}') == 403
            # A reverse proxy or terminal client may name the server however it likes.
            assert request(port, host=rebound) == 200
            # The log names the refused site (one line per path prefix a minute).
            assert request(port, '/v1/agents', host=rebound, Origin=f'http://{rebound}') == 403
            deadline = time.monotonic() + 10
            while True:
                logs = '\n'.join(path.read_text() for path in (state / 'state/data/logs').glob('haarness.log*'))
                if f'origin="{rebound}"' in logs or time.monotonic() > deadline:
                    break
                time.sleep(.1)
            assert f'origin="{rebound}"' in logs and 'fetch_site="cross-site"' in logs, logs[-2000:]
        else:
            # A loopback server already refuses a Host that is not loopback.
            assert request(port, host=f'attacker.example:{port}') == 403

        # The CLI still attaches and runs a turn.
        client = state / 'client'
        client.mkdir()
        output = run([str(binary), '--workspace', str(client), 'run', '--attach', f'http://127.0.0.1:{port}',
                      '--provider', 'echo', '--model', 'echo-1', name + '-cross-site-proof'], env).stdout
        assert 'Echo: ' + name + '-cross-site-proof' in output, output
    print('cross-site E2E passed: loopback open + network anonymous servers refuse page-started and rebound requests; CLI attach works')
finally:
    for workspace, env in servers:
        subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env,
                       capture_output=True, timeout=30)
    e2e_scratch.settle(root)
