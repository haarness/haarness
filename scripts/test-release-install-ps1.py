#!/usr/bin/env python3
"""install.ps1 against a deliberately faulty loopback release service, under
PowerShell 7 (`pwsh`, on any OS) or Windows PowerShell 5.1 (`powershell`).

Every fault leaves an existing haarness.exe and the folder untouched. On
Windows with HAARNESS_BIN set, the real release binary installs, replaces a
running-style previous copy and reports its version.
"""
import hashlib, http.server, os, shutil, subprocess, sys, tempfile, threading
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[1]
SHELL = os.environ.get('HAARNESS_POWERSHELL') or shutil.which('pwsh') or shutil.which('powershell')
state = {'mode': 'corrupt', 'payload': b'', 'version': '0.1.0', 'queries': []}

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        state['queries'].append(parse_qs(urlsplit(self.path).query))
        if state['mode'] == 'absent':
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

def install(env):
    return subprocess.run([SHELL, '-NoProfile', '-NonInteractive', '-Command',
                           f"Get-Content -Raw -LiteralPath '{ROOT / 'install.ps1'}' | Invoke-Expression"],
                          env=env, capture_output=True, text=True, timeout=600)

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
            env = {**os.environ, 'HAARNESS_INSTALL_DIR': str(folder), 'HAARNESS_CHANNEL': 'preview',
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
            binary = os.environ.get('HAARNESS_BIN')
            if os.name == 'nt' and binary:
                state['payload'] = Path(binary).read_bytes()
                state['version'] = subprocess.check_output([binary, '--version'], text=True).split()[-1]
                env['HAARNESS_VERSION'] = state['version']
                result = install(env)
                assert result.returncode == 0, result.stdout + result.stderr
                reported = subprocess.check_output([str(installed), '--version'], text=True).strip()
                assert reported == f"haarness {state['version']}", reported
                assert not (folder / 'haarness.previous.exe').exists()
                print('PASS: the release binary installs over a previous copy and runs')
    finally:
        server.shutdown()
        server.server_close()

if __name__ == '__main__':
    main()
