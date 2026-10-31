#!/usr/bin/env python3
"""install.ps1 against a deliberately faulty loopback release service, under
PowerShell 7 (`pwsh`, on any OS) or Windows PowerShell 5.1 (`powershell`).

Every fault leaves an existing haarness.exe and the folder untouched. On
Windows with HAARNESS_BIN set, the real release binary installs, replaces a
running-style previous copy and reports its version.

Haarness Router goes through the same checks with HAARNESS_PRODUCT=router and
through install-router.ps1, the same script with router as its default product
(the Cloud's one-line substitution, made here as the Cloud makes it). On Windows
with HAARNESS_ROUTER_BIN set, the real router build installs and runs.
"""
import hashlib, http.server, os, shutil, subprocess, sys, tempfile, threading
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[1]
SHELL = os.environ.get('HAARNESS_POWERSHELL') or shutil.which('pwsh') or shutil.which('powershell')
# edge: the Content-Location the service names when asked with HEAD ({port} filled in); None has it refuse HEAD,
# as a service from before the release domain did. The edge, under /edge/, sends the file as R2 does (no checksum
# headers), edge_status instead, or other bytes when edge_corrupt.
state = {'mode': 'corrupt', 'payload': b'', 'version': '0.1.0', 'queries': [], 'paths': [],
         'edge': None, 'edge_status': 200, 'edge_corrupt': False}

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_HEAD(self):
        if state['edge'] is None or state['mode'] != 'ok':
            self.send_error(501)
            return
        self.send_response(200)
        self.send_header('x-content-sha256', hashlib.sha256(state['payload']).hexdigest())
        self.send_header('x-haarness-version', state['version'])
        self.send_header('content-location', state['edge'].format(port=self.server.server_port))
        self.send_header('content-length', str(len(state['payload'])))
        self.end_headers()
    def do_GET(self):
        state['paths'].append(urlsplit(self.path).path)
        if self.path.startswith('/edge/'):
            content = b'MZ other bytes' if state['edge_corrupt'] else state['payload']
            self.send_response(state['edge_status'])
            self.send_header('etag', '"0f3e-3"')
            self.send_header('content-length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
            return
        state['queries'].append(parse_qs(urlsplit(self.path).query))
        if state['mode'] == 'absent' or (state.get('no_desktop_release') and '/desktop/' in self.path):
            body = b'{"code":"not_found","message":"release not found"}'
            self.send_response(404)
            self.send_header('content-type', 'application/json')
            self.send_header('content-length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        content = b'corrupt release' if state['mode'] == 'corrupt' else state['payload']
        self.send_response(200)
        if state['mode'] != 'missing':
            expected = hashlib.sha256(b'expected release' if state['mode'] == 'corrupt' else content).hexdigest()
            self.send_header('x-content-sha256', expected)
        self.send_header('x-haarness-version', state['version'])
        self.send_header('content-length', str(len(content)))
        self.end_headers()
        self.wfile.write(content)

def install(env, script=ROOT / 'install.ps1'):
    return subprocess.run([SHELL, '-NoProfile', '-NonInteractive', '-Command',
                           f"Get-Content -Raw -LiteralPath '{script}' | Invoke-Expression"],
                          env=env, capture_output=True, text=True, timeout=600)

def router_variant(text):
    # install-router.ps1 as the Cloud serves it (haarness-cloud releases.rs): exactly one line changes.
    line = "\n$DefaultProduct = 'all'\n"
    assert text.count(line) == 1, 'install.ps1 must name its default product on one line'
    variant = text.replace(line, "\n$DefaultProduct = 'router'\n")
    changed = [(a, b) for a, b in zip(text.splitlines(), variant.splitlines()) if a != b]
    assert changed == [("$DefaultProduct = 'all'", "$DefaultProduct = 'router'")], changed
    return variant

def main():
    if not SHELL:
        print('SKIP: no pwsh or powershell on PATH')
        return
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='haarness-install-ps1-') as folder:
            folder = Path(folder)
            installed = folder / 'haarness.exe'
            installed.write_bytes(b'existing binary')
            env = {**os.environ, 'HAARNESS_INSTALL_DIR': str(folder), 'HAARNESS_CHANNEL': 'preview', 'HAARNESS_PRODUCT': 'cli',
                   'HAARNESS_RELEASE_URL': f'http://127.0.0.1:{server.server_port}', 'HAARNESS_NO_MODIFY_PATH': '1'}
            # Off Windows there is no registry: the architecture comes from the environment.
            env.setdefault('PROCESSOR_ARCHITECTURE', 'AMD64')
            for mode in ('corrupt', 'missing', 'absent'):
                state['mode'] = mode
                result = install(env)
                assert result.returncode != 0, (mode, result.stdout, result.stderr)
                assert installed.read_bytes() == b'existing binary', mode
                assert sorted(p.name for p in folder.iterdir()) == ['haarness.exe'], mode
            # A platform without a release says so instead of a bare HTTP error.
            assert 'No preview release of Haarness cli for windows x64 yet' in result.stdout + result.stderr, result
            assert state['queries'][-1] == {'os': ['windows'], 'arch': ['x64'], 'kind': ['binary']}, state['queries'][-1]
            print('PASS: corrupt/missing checksum and an unpublished platform rejected; previous binary and clean folder preserved')
            for name, value in (('HAARNESS_RELEASE_URL', 'http://example.com'), ('HAARNESS_CHANNEL', 'nightly')):
                result = install({**env, name: value})
                assert result.returncode != 0 and installed.read_bytes() == b'existing binary', (name, result.stdout)
            print('PASS: plain-HTTP origins and unknown channels refused')
            state['mode'], state['payload'], state['version'] = 'ok', b'MZ not a program', '1.2.3'
            result = install(env)
            assert result.returncode != 0 and 'does not run on this computer' in result.stdout + result.stderr, result.stdout + result.stderr
            assert installed.read_bytes() == b'existing binary'
            print('PASS: a download that cannot run is refused with a plain message')
            # A service that names the file's address on its release domain: the bytes come from there and are checked
            # against the service's checksum ("does not run" comes after the checksum); an edge that does not deliver,
            # or a plain-HTTP address, leaves the service to send the file.
            service = '/api/v1/releases/preview/cli/download'
            for edge, status, corrupt, paths, message in (
                    ('http://127.0.0.1:{port}/edge/cli', 200, False, ['/edge/cli'], 'does not run on this computer'),
                    ('http://127.0.0.1:{port}/edge/cli', 200, True, ['/edge/cli'], 'Checksum mismatch'),
                    ('http://127.0.0.1:{port}/edge/cli', 404, False, ['/edge/cli', service], 'did not deliver'),
                    ('http://example.com/edge/cli', 200, False, [service], 'does not run on this computer')):
                state.update(edge=edge, edge_status=status, edge_corrupt=corrupt, paths=[])
                result = install(env)
                output = result.stdout + result.stderr
                assert result.returncode != 0 and message in output, (edge, status, output)
                assert state['paths'] == paths, (edge, status, state['paths'])
                assert installed.read_bytes() == b'existing binary'
            state.update(edge=None, edge_status=200, edge_corrupt=False)
            print('PASS: files come from the release domain the service names and match its checksum, or from the service')
            # The default installs the CLI, then Desktop; a CLI that fails stops before Desktop is downloaded.
            default = {k: v for k, v in env.items() if k != 'HAARNESS_PRODUCT'}
            state['mode'], state['queries'] = 'corrupt', []
            result = install(default)
            assert result.returncode != 0 and installed.read_bytes() == b'existing binary', result.stdout + result.stderr
            assert all(q.get('kind') == ['binary'] for q in state['queries']), state['queries']
            print('PASS: by default a failed CLI install stops before Desktop')
            binary = os.environ.get('HAARNESS_BIN')
            if os.name == 'nt' and binary:
                state['mode'] = 'ok'
                state['payload'] = Path(binary).read_bytes()
                state['version'] = subprocess.check_output([binary, '--version'], text=True).split()[-1]
                env['HAARNESS_VERSION'] = state['version']
                result = install(env)
                assert result.returncode == 0, result.stdout + result.stderr
                reported = subprocess.check_output([str(installed), '--version'], text=True).strip()
                assert reported == f"haarness {state['version']}", reported
                assert not (folder / 'haarness.previous.exe').exists()
                print('PASS: the release binary installs over a previous copy and runs')
                # By default Desktop follows; without a Desktop release the CLI stays installed and a warning says so.
                state['no_desktop_release'], state['queries'] = True, []
                result = install({**default, 'HAARNESS_VERSION': state['version']})
                state['no_desktop_release'] = False
                assert result.returncode == 0, result.stdout + result.stderr
                assert 'Haarness Desktop was not installed (the CLI is)' in result.stdout + result.stderr, result.stdout + result.stderr
                assert [q.get('kind') for q in state['queries']] == [['binary'], ['exe']], state['queries']
                assert subprocess.check_output([str(installed), '--version'], text=True).strip() == f"haarness {state['version']}"
                print('PASS: the default install reports a missing Desktop and keeps the CLI')
        with tempfile.TemporaryDirectory(prefix='haarness-install-router-ps1-') as folder:
            folder = Path(folder)
            variant = folder.parent / f'{folder.name}-install-router.ps1'
            variant.write_text(router_variant((ROOT / 'install.ps1').read_text()))
            try:
                router_cases(server, folder, variant)
            finally:
                variant.unlink()
    finally:
        server.shutdown()
        server.server_close()

def router_cases(server, folder, variant):
    installed = folder / 'haarness-router.exe'
    installed.write_bytes(b'existing router')
    env = {**os.environ, 'HAARNESS_INSTALL_DIR': str(folder), 'HAARNESS_CHANNEL': 'preview',
           'HAARNESS_RELEASE_URL': f'http://127.0.0.1:{server.server_port}', 'HAARNESS_NO_MODIFY_PATH': '1'}
    env.pop('HAARNESS_PRODUCT', None)
    env.setdefault('PROCESSOR_ARCHITECTURE', 'AMD64')
    service = '/api/v1/releases/preview/router/download'
    state.update(edge=None, edge_status=200, edge_corrupt=False, no_desktop_release=False)
    # Through HAARNESS_PRODUCT=router and through the router variant's own default.
    for script, extra in ((ROOT / 'install.ps1', {'HAARNESS_PRODUCT': 'router'}), (variant, {})):
        for mode, message in (('corrupt', 'Checksum mismatch'), ('missing', 'Checksum mismatch'),
                              ('absent', 'No preview release of Haarness router for windows x64 yet')):
            state.update(mode=mode, version='0.1.0', paths=[], queries=[])
            result = install({**env, **extra}, script)
            output = result.stdout + result.stderr
            assert result.returncode != 0 and message in output, (script.name, mode, output)
            assert state['paths'] == [service] and state['queries'] == [{'os': ['windows'], 'arch': ['x64'], 'kind': ['binary']}], state
            assert installed.read_bytes() == b'existing router' and sorted(p.name for p in folder.iterdir()) == ['haarness-router.exe'], mode
        state.update(mode='ok', payload=b'MZ not a program', version='1.2.3', paths=[])
        result = install({**env, **extra}, script)
        assert result.returncode != 0 and 'The downloaded Haarness Router does not run on this computer' in result.stdout + result.stderr, result
        assert installed.read_bytes() == b'existing router' and state['paths'] == [service]
    print('PASS: HAARNESS_PRODUCT=router and install-router.ps1 fetch the router binary; corrupt, unchecked, unpublished '
          'and broken downloads leave the previous haarness-router.exe and a clean folder')
    state['paths'] = []
    result = install({**env, 'HAARNESS_PRODUCT': 'routers'}, variant)
    assert result.returncode != 0 and 'Invalid release selection' in result.stdout + result.stderr and not state['paths'], result
    print('PASS: an unknown product is refused before any request')
    binary = os.environ.get('HAARNESS_ROUTER_BIN')
    if os.name == 'nt' and binary:
        state['payload'] = Path(binary).read_bytes()
        state['version'] = subprocess.check_output([binary, '--version'], text=True).split()[-1]
        result = install(env, variant)
        assert result.returncode == 0, result.stdout + result.stderr
        assert subprocess.check_output([str(installed), '--version'], text=True).strip() == f"haarness-router {state['version']}"
        assert not (folder / 'haarness-router.previous.exe').exists()
        assert f'Run {installed} to start it' in result.stdout and 'http://127.0.0.1:8080/ui' in result.stdout, result.stdout
        print('PASS: the router build installs over a previous copy, runs, and says how to start it')

if __name__ == '__main__':
    main()
