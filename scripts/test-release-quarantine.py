#!/usr/bin/env python3
"""macOS: verify quarantine removal on downloaded, checksum-verified app contents."""
import hashlib, http.server, os, platform, shutil, subprocess, tempfile, threading
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
if platform.system()!='Darwin': raise SystemExit('macOS-only quarantine test')
with tempfile.TemporaryDirectory(prefix='haarness-quarantine-') as d:
    root=Path(d);app=root/'Haarness.app';(app/'Contents/MacOS').mkdir(parents=True)
    executable=app/'Contents/MacOS/Haarness';executable.write_text('#!/bin/sh\necho quarantine-proof\n');executable.chmod(0o755)
    for p in [app,executable]: subprocess.run(['/usr/bin/xattr','-w','com.apple.quarantine','0083;00000000;ReleaseTest;',str(p)],check=True)
    # A publisher accidentally retaining extended attributes must not break curl installs.
    archive=root/'quarantined.zip'
    subprocess.run(['ditto','-c','-k','--sequesterRsrc','--keepParent',str(app),str(archive)],check=True)
    # Simulate quarantine being propagated by macOS while extracting a downloaded archive.
    # The wrapper only sets attributes; the real system ditto performs extraction.
    wrappers=root/'bin';wrappers.mkdir()
    wrapper=wrappers/'ditto'
    wrapper.write_text('#!/bin/sh\n/usr/bin/ditto "$@" || exit $?\n/usr/bin/xattr -w com.apple.quarantine "0083;00000000;ReleaseTest;" "$4/Haarness.app"\n/usr/bin/xattr -w com.apple.quarantine "0083;00000000;ReleaseTest;" "$4/Haarness.app/Contents/MacOS/Haarness"\n')
    wrapper.chmod(0o755)
    content=archive.read_bytes();checksum=hashlib.sha256(content).hexdigest()
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_GET(self):
            self.send_response(200);self.send_header('x-content-sha256',checksum);self.send_header('x-haarness-version','1.0.0');self.end_headers();self.wfile.write(content)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        installed=root/'installed'
        env={**os.environ,'PATH':str(wrappers)+os.pathsep+os.environ['PATH'],'HAARNESS_RELEASE_URL':f'http://127.0.0.1:{server.server_port}','HAARNESS_CHANNEL':'preview','HAARNESS_PRODUCT':'desktop','HAARNESS_INSTALL_DIR':str(installed)}
        subprocess.run(['sh',str(ROOT/'install.sh')],env=env,check=True)
        attributes=subprocess.check_output(['/usr/bin/xattr','-r',str(installed/'Haarness.app')],text=True)
        assert 'com.apple.quarantine' not in attributes
        assert subprocess.check_output([str(installed/'Haarness.app/Contents/MacOS/Haarness')],text=True).strip()=='quarantine-proof'
        print('PASS: quarantine stripped from verified app and nested executable; executable launches')
    finally: server.shutdown();server.server_close();thread.join()
