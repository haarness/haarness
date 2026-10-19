#!/usr/bin/env python3
"""Installer failure atomicity with a deliberately faulty loopback release service, and the default install of
the CLI and Desktop (skipped with --no-desktop or HAARNESS_NO_DESKTOP=1)."""
import hashlib, http.server, os, platform, pty, shutil, subprocess, tempfile, threading, sys
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
            env={**os.environ,'HAARNESS_INSTALL_DIR':d,'HAARNESS_RELEASE_URL':f'http://127.0.0.1:{server.server_port}','HAARNESS_CHANNEL':'preview','HAARNESS_PRODUCT':'cli'}
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

# The default install: the CLI, then Desktop into its own folder; --no-desktop or HAARNESS_NO_DESKTOP=1 skips
# Desktop; a Desktop failure leaves the CLI installed; Linux without a graphical session skips Desktop.
SCRIPT=Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'install.sh'
macos=platform.system()=='Darwin'
cli_payload=b'#!/bin/sh\nprintf "haarness 2.0.0\\n"\n'
with tempfile.TemporaryDirectory(prefix='haarness-install-both-') as d:
    d=Path(d)
    if macos:
        app=d/'payload'/'Haarness.app'/'Contents'/'MacOS';app.mkdir(parents=True);(app/'Haarness').write_text('#!/bin/sh\n');(app/'Haarness').chmod(0o755)
        subprocess.run(['ditto','-c','-k','--keepParent',str(app.parents[1]),str(d/'desktop.zip')],check=True)
        desktop_payload=(d/'desktop.zip').read_bytes()
    else:
        desktop_payload=b'#!/bin/sh\nexit 1\n'
    state={'queries':[],'desktop':200}
    class Both(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_GET(self):
            path=urlsplit(self.path).path;state['queries'].append(path)
            content=desktop_payload if '/desktop/' in path else cli_payload
            if '/desktop/' in path and state['desktop']!=200:
                self.send_response(state['desktop']);self.end_headers();self.wfile.write(b'{}');return
            self.send_response(200);self.send_header('x-content-sha256',hashlib.sha256(content).hexdigest())
            self.send_header('x-haarness-version','2.0.0');self.send_header('content-length',str(len(content)));self.end_headers();self.wfile.write(content)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Both)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    def run(name,args=(),extra=None,piped=False):
        root=d/name;state['queries'].clear()
        env={k:v for k,v in os.environ.items() if not k.startswith(('HAARNESS_','XDG_','DISPLAY','WAYLAND_DISPLAY'))}
        env.update(HOME=str(root/'home'),XDG_DATA_HOME=str(root/'share'),HAARNESS_RELEASE_URL=f'http://127.0.0.1:{server.server_port}',
                   HAARNESS_CHANNEL='preview',HAARNESS_INSTALL_DIR=str(root/'bin'),HAARNESS_DESKTOP_DIR=str(root/'apps'),DISPLAY=':0')
        env.update(extra or {})
        for key in [k for k,v in env.items() if v is None]: del env[key]
        (root/'home').mkdir(parents=True)
        if piped: r=subprocess.run(['sh','-s','--',*args],input=SCRIPT.read_text(),env=env,capture_output=True,text=True)
        else: r=subprocess.run(['sh',str(SCRIPT),*args],env=env,capture_output=True,text=True)
        return root,r
    desktop_name='Haarness.app' if macos else 'Haarness.AppImage'
    try:
        root,r=run('default')
        assert r.returncode==0,(r.stdout,r.stderr)
        assert subprocess.check_output([str(root/'bin'/'haarness'),'--version'],text=True).strip()=='haarness 2.0.0'
        assert (root/'apps'/desktop_name).exists(),(r.stdout,r.stderr)
        assert macos or (root/'apps'/'haarness-desktop').is_file()
        assert [q.split('/')[-2] for q in state['queries']]==['cli','desktop'],state['queries']
        assert 'Downloading' not in r.stderr,r.stderr
        if macos:
            # An unsigned download is signed ad hoc, so Apple silicon runs it without a developer signature.
            for path in (root/'apps'/desktop_name,root/'bin'/'haarness'):
                subprocess.run(['codesign','--verify','--deep','--strict',str(path)],check=True)
        print('PASS: the default install puts the CLI and Desktop in their folders'+(', signed ad hoc' if macos else ''))
        # On a terminal (stderr stays one under `curl | sh`) each download says what it is and shows progress.
        leader,follower=pty.openpty();root=d/'progress';state['queries'].clear()
        env={k:v for k,v in os.environ.items() if not k.startswith(('HAARNESS_','XDG_'))}
        env.update(HOME=str(root/'home'),HAARNESS_RELEASE_URL=f'http://127.0.0.1:{server.server_port}',HAARNESS_CHANNEL='preview',
                   HAARNESS_INSTALL_DIR=str(root/'bin'),HAARNESS_PRODUCT='cli')
        (root/'home').mkdir(parents=True)
        shown=[]
        def drain():
            while True:
                try: chunk=os.read(leader,65536)
                except OSError: return
                if not chunk: return
                shown.append(chunk)
        reader=threading.Thread(target=drain,daemon=True);reader.start()
        r=subprocess.run(['sh','-s'],input=SCRIPT.read_bytes(),env=env,stdout=subprocess.PIPE,stderr=follower)
        os.close(follower);reader.join(5);os.close(leader);shown=b''.join(shown)
        assert r.returncode==0 and b'Downloading Haarness CLI for ' in shown and b'100.0%' in shown,(r.returncode,shown)
        print('PASS: on a terminal the installer names each download and shows its progress')
        for name,args,extra,piped in (('flag',['--no-desktop'],None,False),('piped',['--no-desktop'],None,True),('env',[],{'HAARNESS_NO_DESKTOP':'1'},False)):
            root,r=run(name,args,extra,piped)
            assert r.returncode==0,(name,r.stdout,r.stderr)
            assert (root/'bin'/'haarness').is_file() and not (root/'apps').exists(),name
            assert all('/desktop/' not in q for q in state['queries']),(name,state['queries'])
        print('PASS: --no-desktop (also through sh -s --) and HAARNESS_NO_DESKTOP=1 install only the CLI')
        state['desktop']=404
        root,r=run('desktop-missing')
        state['desktop']=200
        assert r.returncode==0,(r.stdout,r.stderr)
        assert (root/'bin'/'haarness').is_file() and not (root/'apps'/desktop_name).exists()
        assert 'Haarness Desktop was not installed (the CLI is)' in r.stderr and 'No preview release of Haarness desktop' in r.stderr,r.stderr
        print('PASS: a Desktop failure is reported and leaves the CLI installed')
        if not macos:
            root,r=run('headless',extra={'DISPLAY':None})
            assert r.returncode==0 and 'No graphical session' in r.stdout,(r.stdout,r.stderr)
            assert all('/desktop/' not in q for q in state['queries']) and not (root/'apps').exists(),state['queries']
            print('PASS: Linux without a graphical session installs only the CLI and says how to add Desktop')
        root,r=run('unknown',['--desktop-only'])
        assert r.returncode!=0 and 'Unknown option' in r.stderr and not (root/'bin').exists(),(r.stdout,r.stderr)
        print('PASS: an unknown option is refused before anything is installed')
    finally: server.shutdown();server.server_close();thread.join()
