#!/usr/bin/env python3
"""Installer failure atomicity with a deliberately faulty loopback release service."""
import hashlib, http.server, os, subprocess, tempfile, threading
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_GET(self):
        content=b'corrupt release'
        self.send_response(200)
        if mode!='missing': self.send_header('x-content-sha256',hashlib.sha256(b'expected release').hexdigest())
        self.send_header('x-haarness-version','0.1.0');self.end_headers();self.wfile.write(content)
with tempfile.TemporaryDirectory(prefix='haarness-install-test-') as d:
    installed=Path(d)/'haarness';installed.write_bytes(b'existing binary');installed.chmod(0o755)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        for mode in ['corrupt','missing']:
            env={**os.environ,'HAARNESS_INSTALL_DIR':d,'HAARNESS_RELEASE_URL':f'http://127.0.0.1:{server.server_port}','HAARNESS_CHANNEL':'preview'}
            r=subprocess.run(['sh',str(ROOT/'install.sh')],env=env,capture_output=True,text=True)
            assert r.returncode!=0,(mode,r.stdout,r.stderr)
            assert installed.read_bytes()==b'existing binary'
            assert sorted(p.name for p in Path(d).iterdir())==['haarness']
        print('PASS: corrupt/missing checksum rejected; previous binary and clean staging preserved')
    finally: server.shutdown();server.server_close();thread.join()
