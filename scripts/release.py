#!/usr/bin/env python3
"""Collect real build outputs, publish immutably, and verify every downloaded byte.
No npm/Docker publication. Tokens are read from the environment, never arguments.
"""
import argparse, hashlib, http.client, json, os, platform, shutil, subprocess, tempfile
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit, urlencode
ROOT = Path(__file__).resolve().parents[1]

def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024*1024), b''): h.update(chunk)
    return h.hexdigest()

def gatekeeper(package):
    # What a Mac makes of the Haarness.app in a downloaded .dmg or .zip. Browsers quarantine downloads, and Gatekeeper
    # opens a quarantined app only if it is notarized Developer ID: 0.1.1 and 0.1.2 shipped ad-hoc signed, and macOS
    # called them damaged and offered to move them to the Trash. The stapled ticket lets the first launch work offline.
    if platform.system()!='Darwin': raise ValueError(f'{package.name}: macOS packages are checked on macOS')
    def run(*cmd): r=subprocess.run(cmd,capture_output=True,text=True);return r.returncode,(r.stdout+r.stderr).strip()
    with tempfile.TemporaryDirectory(prefix='haarness-gatekeeper-') as d:
        root=Path(d)/'contents';mounted=package.suffix=='.dmg'
        # The disk image is assessed on its own when a browser-downloaded copy opens: signed, notarized, stapled too.
        if mounted:
            code,out=run('spctl','--assess','--type','open','--context','context:primary-signature','-vv',str(package))
            if code or 'source=Notarized Developer ID' not in out: raise ValueError(f'{package.name}: Gatekeeper rejects the disk image: {out}')
            code,out=run('xcrun','stapler','validate',str(package))
            if code: raise ValueError(f'{package.name}: no notarization ticket stapled to the disk image: {out}')
            subprocess.run(['hdiutil','attach','-nobrowse','-readonly','-noautoopen','-mountpoint',str(root),str(package)],check=True,stdout=subprocess.DEVNULL)
        else: subprocess.run(['ditto','-x','-k',str(package),str(root)],check=True)
        try:
            apps=sorted(root.glob('*.app'))
            if len(apps)!=1: raise ValueError(f'{package.name}: expected one app, found {[a.name for a in apps]}')
            app=str(apps[0])
            code,out=run('codesign','--verify','--deep','--strict',app)
            if code: raise ValueError(f'{package.name}: broken signature: {out}')
            _,out=run('codesign','-dv','--verbose=2',app);lines=out.splitlines()
            signer=next((l.split('=',1)[1] for l in lines if l.startswith('Authority=')),'ad-hoc')
            team=next((l.split('=',1)[1] for l in lines if l.startswith('TeamIdentifier=')),None)
            if not signer.startswith('Developer ID Application: '): raise ValueError(f'{package.name}: signed {signer}, not with a Developer ID Application certificate')
            code,out=run('xcrun','stapler','validate',app)
            if code: raise ValueError(f'{package.name}: no notarization ticket stapled: {out}')
            code,out=run('spctl','--assess','--type','execute','-vv',app)
            if code or 'source=Notarized Developer ID' not in out: raise ValueError(f'{package.name}: Gatekeeper rejects it: {out}')
            return dict(signer=signer,team=team,source='Notarized Developer ID')
        finally:
            if mounted: subprocess.run(['hdiutil','detach','-force',str(root)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)

def check_macos(args):
    for p in args.packages:
        g=gatekeeper(Path(p));print(f'{Path(p).name}: {g["signer"]}, {g["source"]}')

def collect(args):
    revision = subprocess.check_output(['git','rev-parse','HEAD'], cwd=ROOT, text=True).strip()
    dirty = subprocess.check_output(['git','status','--porcelain','--untracked-files=no'], cwd=ROOT, text=True).strip()
    if dirty and not args.allow_dirty: raise ValueError('Commit build sources before collecting a publishable release (or --allow-dirty for local proof).')
    # The build host's platform, unless --os/--arch name the target of a cross build (Linux arm64 or Windows from
    # a Linux x64 host when CI is unavailable). A cross-built binary cannot run here: --version names it then.
    os_name = args.os or {'Darwin':'macos','Linux':'linux','Windows':'windows'}[platform.system()]
    # Windows reports ARM64 in capitals (and AMD64), macOS arm64, Linux aarch64.
    arch = args.arch or {'arm64':'arm64','aarch64':'arm64','amd64':'x64','x86_64':'x64'}[platform.machine().lower()]
    binary = Path(args.binary).resolve()
    version = args.version or subprocess.check_output([str(binary),'--version'],text=True).strip().removeprefix('haarness ')
    desktop = json.loads((ROOT/'apps/desktop/package.json').read_text())
    if desktop['version'] != version: raise ValueError('CLI and Desktop versions differ')
    out = Path(args.out).resolve(); out.mkdir(parents=True, exist_ok=True)
    artifacts = []
    def add(path, product, kind, name=None):
        name = name or path.name
        dest = out/name
        if path.resolve() != dest: shutil.copy2(path,dest)
        artifact=dict(file=name, product=product, kind=kind, os=os_name, arch=arch, size=dest.stat().st_size, sha256=digest(dest))
        # Recorded for a publisher that is not a Mac (CI publishes from Linux); publish refuses a package without it.
        if os_name=='macos' and product=='desktop':
            try: artifact['gatekeeper']=gatekeeper(dest)
            except ValueError as e: print(f'Warning, publish will refuse this package: {e}')
        artifacts.append(artifact)
    add(binary,'cli','binary',f'haarness-{os_name}-{arch}'+('.exe' if os_name=='windows' else ''))
    extensions = {'macos':{'dmg','zip'},'linux':{'AppImage','deb'},'windows':{'exe'}}[os_name]
    build_os = {'macos':'mac','linux':'linux','windows':'win'}[os_name]
    # electron-builder names Linux packages by each format's word for the CPU
    # (x86_64 for AppImage, amd64 for .deb); publish every package as x64/arm64.
    arch_names = {'x64':{'x64','x86_64','amd64'},'arm64':{'arm64','aarch64'}}[arch]
    for path in sorted(Path(args.desktop_dir).iterdir()):
        kind = path.suffix.lstrip('.')
        name = f'Haarness-{version}-{build_os}-{arch}.{kind}'
        if path.is_file() and kind in extensions and any(path.name == f'Haarness-{version}-{build_os}-{a}.{kind}' for a in arch_names): add(path,'desktop',kind,name)
    if not any(a['product']=='desktop' for a in artifacts): raise ValueError('No Desktop packages found for this version and architecture')
    index = dict(version=version, sourceRevision=revision, dirty=bool(dirty), releasedAt=datetime.now(timezone.utc).isoformat(), artifacts=artifacts)
    path=out/f'index-{os_name}-{arch}.json';path.write_text(json.dumps(index,indent=2)+'\n')
    print(path)

def connection(base):
    u=urlsplit(base)
    if u.username or u.password or u.query or u.fragment or u.path not in ('','/'): raise ValueError('Use a release service origin')
    if u.scheme=='https': return http.client.HTTPSConnection(u.hostname,u.port,timeout=900)
    if u.scheme=='http' and u.hostname in ('127.0.0.1','localhost','::1'): return http.client.HTTPConnection(u.hostname,u.port,timeout=900)
    raise ValueError('Release service requires HTTPS or loopback HTTP')

def request(base,method,path,body=None,headers=None):
    c=connection(base)
    headers=dict(headers or {})
    if method!='GET': headers['x-haarness-internal-token']=os.environ['HAARNESS_RELEASE_TOKEN']
    if isinstance(body,dict): body=json.dumps(body).encode();headers['content-type']='application/json'
    c.request(method,path,body=body,headers=headers);r=c.getresponse();data=r.read();c.close()
    if r.status>=300: raise RuntimeError(f'{method} {path}: {r.status} {data[:1500].decode(errors="replace")}')
    return json.loads(data) if data else None

def publish(args):
    if args.allow_unsigned and args.channel=='stable': raise ValueError('Stable never ships a Mac Desktop that Gatekeeper rejects')
    indexes=[(Path(p),json.loads(Path(p).read_text())) for p in args.index]
    first=indexes[0][1]; ids=[]; seen=set(); uploads=[]
    # Every artifact is checked before the first upload, so a refused release leaves nothing behind.
    for path,index in indexes:
        if index.get('dirty') and not (args.allow_dirty and urlsplit(args.url).hostname in ('127.0.0.1','localhost')): raise ValueError('Dirty artifacts can only be used for explicit local proof')
        if any(index[k]!=first[k] for k in ('version','sourceRevision')): raise ValueError('All builds must share version and source commit')
        for a in index['artifacts']:
            file=path.parent/a['file']
            if file.parent.resolve()!=path.parent.resolve() or digest(file)!=a['sha256'] or file.stat().st_size!=a['size']: raise ValueError(f'Artifact changed: {file}')
            identity=(a['product'],a['os'],a['arch'],a['kind'])
            if identity in seen: raise ValueError(f'Duplicate artifact: {identity}')
            seen.add(identity)
            # A Mac publisher checks the very bytes it uploads; elsewhere collect's record on the build Mac stands in.
            if a['os']=='macos' and a['product']=='desktop' and not args.allow_unsigned:
                if platform.system()=='Darwin': gatekeeper(file)
                elif not a.get('gatekeeper'): raise ValueError(f'{a["file"]}: no Gatekeeper record; collect it on a Mac that signs with Developer ID and notarizes')
            uploads.append((file,a))
    for file,a in uploads:
        q={k:a[k] for k in ('product','file','kind','os','arch')};q['version']=first['version']
        with file.open('rb') as stream:
            saved=request(args.url,'POST','/internal/releases/artifacts?'+urlencode(q),stream,{'Content-Length':str(a['size']),'Content-Type':'application/octet-stream','x-content-sha256':a['sha256']})
        ids.append(saved['id']);print(f'Uploaded {a["file"]}: {a["sha256"]}')
    body=dict(channel=args.channel,version=first['version'],sourceRevision=first['sourceRevision'],releasedAt=first['releasedAt'],artifacts=ids,notesUrl=args.notes_url)
    result=request(args.url,'POST','/internal/releases',body)
    # Publication already happened. Retain the receipt even if public delivery
    # fails, so operators can inspect or roll back the exact publication.
    Path(args.receipt).write_text(json.dumps(result,indent=2)+'\n')
    for product in result['release']['manifest']['products'].values():
        for a in product['artifacts']:
            # Test the same URL the frontend gives users, including its query.
            # Never attach the operator token to a public artifact request.
            u=urlsplit(a['url'])
            if u.username or u.password or u.fragment: raise ValueError('Invalid public artifact URL')
            c=connection(urlunsplit((u.scheme,u.netloc,'','','')))
            try:
                c.request('GET',urlunsplit(('','',u.path or '/',u.query,'')));r=c.getresponse()
                if r.status!=200: raise RuntimeError(f'Public download failed: {r.status} ({u.hostname})')
                h=hashlib.sha256();size=0
                while chunk:=r.read(1024*1024):h.update(chunk);size+=len(chunk)
            finally:
                c.close()
            if h.hexdigest()!=a['digest']['sha256'] or size!=a['size']: raise ValueError('Published download integrity failure')
            print(f'Verified public download: {a["file"]} ({size} bytes)')
    print('Release ID:',result['id'])

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
    c=sub.add_parser('collect');c.add_argument('--binary',default=str(ROOT/'target/release'/('haarness.exe' if os.name=='nt' else 'haarness')));c.add_argument('--out',default=str(ROOT/'dist/releases'));c.add_argument('--allow-dirty',action='store_true');c.add_argument('--os',choices=['macos','linux','windows'],help='target OS of a cross build');c.add_argument('--arch',choices=['x64','arm64'],help='target architecture of a cross build');c.add_argument('--version',help='version of a cross-built binary that cannot run here');c.add_argument('--desktop-dir',default=str(ROOT/'apps/desktop/release'),help='where electron-builder wrote the packages');c.set_defaults(run=collect)
    c=sub.add_parser('publish');c.add_argument('--index',nargs='+',required=True);c.add_argument('--url',required=True);c.add_argument('--channel',choices=['preview','stable'],default='preview');c.add_argument('--notes-url');c.add_argument('--receipt',default='release-receipt.json');c.add_argument('--allow-dirty',action='store_true');c.add_argument('--allow-unsigned',action='store_true',help='preview only: publish a Mac Desktop that Gatekeeper rejects');c.set_defaults(run=publish)
    c=sub.add_parser('check-macos',help='check that Gatekeeper accepts the app in each downloaded .dmg/.zip');c.add_argument('packages',nargs='+');c.set_defaults(run=check_macos)
    args=p.parse_args();args.run(args)
