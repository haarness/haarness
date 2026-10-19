#!/usr/bin/env python3
"""Installer failure atomicity with a deliberately faulty loopback release service."""
import hashlib, http.server, os, shutil, subprocess, tempfile, threading, sys
from urllib.parse import parse_qs, urlsplit
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_GET(self):
        if mode == 'absent':
            body=b'{"code":"not_found","message":"release not found"}'
            self.send_response(404);self.send_header('content-type','application/json');self.end_headers();self.wfile.write(body);return
        content=b'corrupt release'
        if mode == 'metadata':
            if parse_qs(urlsplit(self.path).query).get('version') != ['1.2.3+build.4']:
                self.send_error(404); return
            content=b'#!/bin/sh\nprintf \"haarness 1.2.3+build.4\\n\"\n'
        self.send_response(200)
        if mode!='missing': self.send_header('x-content-sha256',hashlib.sha256(content if mode == 'metadata' else b'expected release').hexdigest())
        self.send_header('x-haarness-version','1.2.3+build.4' if mode == 'metadata' else '0.1.0');self.end_headers();self.wfile.write(content)
with tempfile.TemporaryDirectory(prefix='haarness-install-test-') as d:
    installed=Path(d)/'haarness';installed.write_bytes(b'existing binary');installed.chmod(0o755)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        for mode in ['corrupt','missing','absent']:
            env={**os.environ,'HAARNESS_INSTALL_DIR':d,'HAARNESS_RELEASE_URL':f'http://127.0.0.1:{server.server_port}','HAARNESS_CHANNEL':'preview'}
            r=subprocess.run(['sh',str(Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'install.sh')],env=env,capture_output=True,text=True)
            assert r.returncode!=0,(mode,r.stdout,r.stderr)
            assert installed.read_bytes()==b'existing binary'
            assert sorted(p.name for p in Path(d).iterdir())==['haarness']
        # A platform without a release says so instead of curl's bare "error: 404".
        assert 'No preview release of Haarness cli for ' in r.stderr and '404' not in r.stderr,r.stderr
        mode='metadata'
        env['HAARNESS_VERSION']='1.2.3+build.4'
        r=subprocess.run(['sh',str(Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'install.sh')],env=env,capture_output=True,text=True)
        assert r.returncode==0,(r.stdout,r.stderr)
        assert subprocess.check_output([str(installed),'--version'],text=True).strip()=='haarness 1.2.3+build.4'
        print('PASS: pinned SemVer build metadata installs and executes')
        if os.environ.get('HAARNESS_BIN'):
            shutil.copy2(os.environ['HAARNESS_BIN'], installed)
            r=subprocess.run([str(installed),'upgrade','--channel','preview','--version','1.2.3+build.4','--release-url',env['HAARNESS_RELEASE_URL']],env=env,capture_output=True,text=True)
            assert r.returncode==0,(r.stdout,r.stderr)
            assert subprocess.check_output([str(installed),'--version'],text=True).strip()=='haarness 1.2.3+build.4'
            print('PASS: built CLI upgrades to a pinned SemVer build-metadata version')
        print('PASS: corrupt/missing checksum and an unpublished platform rejected; previous binary and clean staging preserved')
    finally: server.shutdown();server.server_close();thread.join()
