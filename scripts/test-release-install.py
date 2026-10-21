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
    # cuts and etags apply to the next responses in turn: where each stops early, and what it names its file
    # (None: no ETag). edge is the Content-Location the service names when asked with HEAD ({port} and {product}
    # filled in); None has it refuse HEAD, as a service from before the release domain did. The edge, under /edge/,
    # sends the file as R2 does: its own ETag and no checksum headers, or edge_status instead.
    state={'queries':[],'heads':[],'desktop':200,'cuts':[],'etags':[],'ranges':[],'edge':None,'edge_status':200}
    class Both(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_HEAD(self):
            path=urlsplit(self.path).path;state['heads'].append(path)
            if state['edge'] is None: self.send_error(501);return
            content=desktop_payload if '/desktop/' in path else cli_payload
            self.send_response(200);self.send_header('x-content-sha256',hashlib.sha256(content).hexdigest());self.send_header('x-haarness-version','2.0.0')
            self.send_header('content-location',state['edge'].format(port=self.server.server_port,product=path.split('/')[-2]))
            self.send_header('content-length',str(len(content)));self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path;state['queries'].append(path);state['ranges'].append(self.headers.get('range'))
            edge=path.startswith('/edge/')
            content=desktop_payload if '/desktop/' in path else cli_payload
            if edge and state['edge_status']!=200:
                self.send_response(state['edge_status']);self.end_headers();self.wfile.write(b'<Error/>');return
            if edge and state.get('edge_corrupt'): content=b'#!/bin/sh\nprintf "haarness 2.0.0\\n"\n# changed\n'
            if '/desktop/' in path and state['desktop']!=200:
                self.send_response(state['desktop']);self.end_headers();self.wfile.write(b'{}');return
            sha=hashlib.sha256(content).hexdigest();etag=state['etags'].pop(0) if state['etags'] else ('"0f3e-3"' if edge else f'"{sha}"')
            start=0
            if self.headers.get('range') and self.headers.get('if-range') in (None,etag):
                start=int(self.headers['range'].removeprefix('bytes=').rstrip('-'))
            self.send_response(206 if start else 200)
            if not edge: self.send_header('x-content-sha256',sha);self.send_header('x-haarness-version','2.0.0')
            if etag: self.send_header('etag',etag)
            if start: self.send_header('content-range',f'bytes {start}-{len(content)-1}/{len(content)}')
            self.send_header('content-length',str(len(content)-start));self.end_headers()
            cut=state['cuts'].pop(0) if state['cuts'] else len(content)
            self.wfile.write(content[start:start+cut])
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
        # An interrupted download continues from where it stopped; a file that changed meanwhile, or one the service
        # named no ETag for, is downloaded again from the start.
        for name,etags,ranges in (('resume',[],[None,'bytes=10-']),('changed',['"old"'],[None,'bytes=10-',None]),('no-etag',[None],[None,None])):
            state['cuts']=[10];state['etags']=etags;state['ranges'].clear()
            root,r=run(name,extra={'HAARNESS_PRODUCT':'cli'})
            assert r.returncode==0,(name,r.stdout,r.stderr)
            assert subprocess.check_output([str(root/'bin'/'haarness'),'--version'],text=True).strip()=='haarness 2.0.0',name
            assert state['ranges']==ranges and 'trying again (2 of 3)' in r.stderr,(name,state['ranges'],r.stderr)
        print('PASS: an interrupted download continues where it stopped, or starts over for a changed or unnamed file')
        state['cuts']=[10,10,10]
        root,r=run('interrupted',extra={'HAARNESS_PRODUCT':'cli'})
        assert r.returncode!=0 and 'Could not reach the Haarness release service' in r.stderr,(r.stdout,r.stderr)
        assert len(state['queries'])==3 and not (root/'bin'/'haarness').exists(),state['queries']
        print('PASS: three interrupted tries give up and install nothing')
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
        # A service that names the file's address on its release domain: the bytes come from there, the checksum and
        # version from the service, and an interrupted download continues on the release domain.
        address='http://127.0.0.1:{port}/edge/{product}/file'
        service='/api/v1/releases/preview/cli/download'
        state['edge']=address
        for name,cuts,ranges in (('edge',[],[None]),('edge-resume',[10],[None,'bytes=10-'])):
            state['cuts']=cuts;state['ranges'].clear();state['heads'].clear()
            root,r=run(name,extra={'HAARNESS_PRODUCT':'cli'})
            assert r.returncode==0,(name,r.stdout,r.stderr)
            assert subprocess.check_output([str(root/'bin'/'haarness'),'--version'],text=True).strip()=='haarness 2.0.0',name
            assert state['heads']==[service] and state['queries']==['/edge/cli/file']*len(ranges),(name,state)
            assert state['ranges']==ranges,(name,state['ranges'],r.stderr)
        root,r=run('edge-both')
        assert r.returncode==0 and (root/'apps'/desktop_name).exists(),(r.stdout,r.stderr)
        assert state['queries']==['/edge/cli/file','/edge/desktop/file'],state['queries']
        print('PASS: files come from the release domain the service names, and an interrupted one continues there')
        # A release domain that does not deliver, and an address that is not HTTPS, leave the service to send the file.
        for name,edge,status,queries in (('edge-missing',address,404,['/edge/cli/file',service]),
                                         ('edge-plain-http','http://example.com/{product}',200,[service])):
            state['edge'],state['edge_status']=edge,status
            root,r=run(name,extra={'HAARNESS_PRODUCT':'cli'})
            assert r.returncode==0,(name,r.stdout,r.stderr)
            assert subprocess.check_output([str(root/'bin'/'haarness'),'--version'],text=True).strip()=='haarness 2.0.0',name
            assert state['queries']==queries,(name,state['queries'])
            assert ('did not deliver' in r.stderr)==(status!=200),(name,r.stderr)
        state['edge'],state['edge_corrupt']=address,True
        root,r=run('edge-corrupt',extra={'HAARNESS_PRODUCT':'cli'})
        assert r.returncode!=0 and 'Checksum mismatch' in r.stderr and not (root/'bin'/'haarness').exists(),(r.stdout,r.stderr)
        state['edge'],state['edge_status'],state['edge_corrupt']=None,200,False
        print('PASS: an edge that does not deliver, or a plain-HTTP address, falls back to the release service; '
              'bytes from the edge that do not match the service\'s checksum are refused')
    finally: server.shutdown();server.server_close();thread.join()
