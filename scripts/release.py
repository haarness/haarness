#!/usr/bin/env python3
"""Collect real build outputs, publish immutably, and verify every downloaded byte.
No npm/Docker publication. Tokens are read from the environment, never arguments.
"""
import argparse, base64, hashlib, http.client, json, os, platform, shutil, subprocess, tempfile, zipfile
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

# Haarness for Chrome: the ID its manifest key pins (docs/CHROME-EXTENSION.md), and where the release owner keeps the
# private key beside the checkouts.
EXTENSION_ID='accdjhmcgljbadpeanajllmcembhkomn'
EXTENSION_KEY=ROOT.parent/'.chrome-extension-key'/'key.pem'

def extension_id(public_der):
    # Chrome's ID for a public key: the first 32 hex digits of its SHA-256, written with the letters a-p.
    return ''.join(chr(97+int(c,16)) for c in hashlib.sha256(public_der).hexdigest()[:32])

def varint(n):
    out=b''
    while True:
        byte=n&0x7f;n>>=7
        if not n: return out+bytes([byte])
        out+=bytes([byte|0x80])

def field(number,data): return varint(number<<3|2)+varint(len(data))+data

def fields(data):
    # The length-delimited fields of a protobuf message, as (number, bytes) pairs; CRX headers use no other kind.
    out=[];i=0
    def read():
        nonlocal i
        n=shift=0
        while True:
            b=data[i];i+=1;n|=(b&0x7f)<<shift;shift+=7
            if not b&0x80: return n
    while i<len(data):
        key=read()
        if key&7!=2: raise ValueError('Unexpected protobuf wire type in the CRX header')
        size=read();out.append((key>>3,data[i:i+size]));i+=size
    return out

def crx3(archive,key):
    # A CRX3 package (Chromium's components/crx_file/crx3.proto): "Cr24", version 3, the header size, a header holding
    # the RSA public key and its SHA-256 signature over the signed data, then the zip itself.
    public=subprocess.run(['openssl','pkey','-in',str(key),'-pubout','-outform','DER'],capture_output=True,check=True).stdout
    signed_data=field(1,hashlib.sha256(public).digest()[:16])
    message=b'CRX3 SignedData\x00'+len(signed_data).to_bytes(4,'little')+signed_data+archive
    signature=subprocess.run(['openssl','dgst','-sha256','-sign',str(key)],input=message,capture_output=True,check=True).stdout
    header=field(2,field(1,public)+field(2,signature))+field(10000,signed_data)
    return b'Cr24'+(3).to_bytes(4,'little')+len(header).to_bytes(4,'little')+header+archive

def verify_crx(package):
    # The archive inside a CRX3 whose signature verifies and whose ID is Haarness for Chrome's.
    data=Path(package).read_bytes()
    if data[:4]!=b'Cr24' or int.from_bytes(data[4:8],'little')!=3: raise ValueError(f'{package}: not a CRX3 package')
    size=int.from_bytes(data[8:12],'little');header=dict(fields(data[12:12+size]));archive=data[12+size:]
    proof=dict(fields(header[2]));public,signature=proof[1],proof[2]
    signed_data=header[10000]
    if dict(fields(signed_data))[1]!=hashlib.sha256(public).digest()[:16] or extension_id(public)!=EXTENSION_ID: raise ValueError(f'{package}: signed for another extension ID')
    message=b'CRX3 SignedData\x00'+len(signed_data).to_bytes(4,'little')+signed_data+archive
    with tempfile.TemporaryDirectory(prefix='haarness-crx-') as d:
        der=Path(d)/'public.der';der.write_bytes(public);sig=Path(d)/'signature';sig.write_bytes(signature)
        pem=subprocess.run(['openssl','pkey','-pubin','-inform','DER','-in',str(der)],capture_output=True,check=True).stdout
        (Path(d)/'public.pem').write_bytes(pem)
        r=subprocess.run(['openssl','dgst','-sha256','-verify',str(Path(d)/'public.pem'),'-signature',str(sig)],input=message,capture_output=True)
        if r.returncode: raise ValueError(f'{package}: bad signature')
    return archive

def collect_extension(args):
    # Haarness for Chrome for the release service: its unpacked folder as a zip people load, and the same zip signed
    # as a CRX that Chrome installs by policy. One build for every platform (os and arch "any").
    folder=Path(args.extension_dir).resolve();manifest=json.loads((folder/'manifest.json').read_text())
    version=manifest.get('version_name')
    if args.version and version!=args.version: raise ValueError(f'The extension is {version}, not {args.version}')
    if manifest.get('manifest_version')!=3: raise ValueError('Not a Manifest V3 extension')
    public=base64.b64decode(manifest.get('key',''))
    if extension_id(public)!=EXTENSION_ID: raise ValueError(f'The manifest key gives {extension_id(public)}, not {EXTENSION_ID}')
    key=Path(args.key)
    if not key.is_file(): raise ValueError(f'No signing key at {key}: the release owner keeps it outside every repository')
    signing=subprocess.run(['openssl','pkey','-in',str(key),'-pubout','-outform','DER'],capture_output=True,check=True).stdout
    if signing!=public: raise ValueError('The signing key is not the one the manifest pins')
    files=sorted(p for p in folder.rglob('*') if p.is_file() or p.is_symlink())
    if any(p.is_symlink() for p in files): raise ValueError('Links are not packaged')
    if leaked:=[p.relative_to(folder).as_posix() for p in files if p.suffix in ('.ts','.tsx','.map')]: raise ValueError(f'Source files in the build: {leaked}')
    if args.revision: revision=args.revision;dirty=''
    else:
        revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()
        dirty=subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=ROOT,text=True).strip()
        if dirty and not args.allow_dirty: raise ValueError('Commit build sources before collecting a publishable release (or --allow-dirty for local proof).')
    if len(revision)!=40 or any(c not in '0123456789abcdef' for c in revision): raise ValueError('--revision takes a full Git commit')
    out=Path(args.out).resolve();out.mkdir(parents=True,exist_ok=True)
    # Sorted names, one timestamp and fixed modes: the same folder always gives the same bytes.
    zip_path=out/f'haarness-chrome-{version}.zip'
    with zipfile.ZipFile(zip_path,'w',zipfile.ZIP_DEFLATED,compresslevel=9) as z:
        for p in files:
            info=zipfile.ZipInfo(p.relative_to(folder).as_posix(),date_time=(1980,1,1,0,0,0));info.external_attr=0o644<<16
            z.writestr(info,p.read_bytes(),zipfile.ZIP_DEFLATED,9)
    crx_path=out/f'haarness-chrome-{version}.crx';crx_path.write_bytes(crx3(zip_path.read_bytes(),key))
    if verify_crx(crx_path)!=zip_path.read_bytes(): raise ValueError('The CRX does not carry the zip')
    artifacts=[dict(file=p.name,product='chrome-extension',kind=p.suffix[1:],os='any',arch='any',size=p.stat().st_size,sha256=digest(p)) for p in (zip_path,crx_path)]
    index=dict(version=version,sourceRevision=revision,dirty=bool(dirty),releasedAt=datetime.now(timezone.utc).isoformat(),artifacts=artifacts)
    path=out/'index-chrome-extension.json';path.write_text(json.dumps(index,indent=2)+'\n')
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
        # The files this run uploaded: all of a new release's, or only the product a published one gained.
        for a in (a for a in product['artifacts'] if a['id'] in ids):
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

def gh(*args):
    r=subprocess.run(['gh',*args],capture_output=True,text=True)
    if r.returncode: raise RuntimeError(f'gh {" ".join(args[:3])}: {r.stderr.strip()[:1500]}')
    return json.loads(r.stdout) if r.stdout.strip() else None

def github(args):
    # Mirrors a publication onto the tag's GitHub release, with the bytes the indexes record and a SHA256SUMS file.
    # The release stays a draft until GitHub reports every asset uploaded with the expected size and SHA-256, so it
    # never shows a partial set; a published release is never changed, only checked (and a preview made stable).
    indexes=[(Path(p),json.loads(Path(p).read_text())) for p in args.index]
    first=indexes[0][1];files={}
    for path,index in indexes:
        if index.get('dirty'): raise ValueError('Dirty artifacts are not released on GitHub')
        if any(index[k]!=first[k] for k in ('version','sourceRevision')): raise ValueError('All builds must share version and source commit')
        for a in index['artifacts']:
            file=path.parent/a['file']
            if file.parent.resolve()!=path.parent.resolve() or digest(file)!=a['sha256'] or file.stat().st_size!=a['size']: raise ValueError(f'Artifact changed: {file}')
            if a['file'] in files or a['file']=='SHA256SUMS': raise ValueError(f'Duplicate asset name: {a["file"]}')
            files[a['file']]=(file,a['size'],a['sha256'])
    repo,tag=args.repo,f'v{first["version"]}'
    if gh('api',f'repos/{repo}/commits/{tag}')['sha']!=first['sourceRevision']: raise ValueError(f'{repo} {tag} is not {first["sourceRevision"]}')
    with tempfile.TemporaryDirectory(prefix='haarness-github-') as d:
        sums=Path(d)/'SHA256SUMS';sums.write_text(''.join(f'{h}  {n}\n' for n,(_,_,h) in sorted(files.items())))
        files['SHA256SUMS']=(sums,sums.stat().st_size,digest(sums))
        found=[r for page in gh('api','--paginate','--slurp',f'repos/{repo}/releases') for r in page if r['tag_name']==tag]
        if len(found)>1: raise ValueError(f'{repo} has {len(found)} releases for {tag}')
        release=found[0] if found else None
        if release is None:
            notes=f'Haarness {first["version"]}, built from {first["sourceRevision"]}. The same files as https://haarness.ai/download; SHA-256 sums in SHA256SUMS.'+(f'\n\n{args.notes_url}' if args.notes_url else '')
            release=gh('api','-X','POST',f'repos/{repo}/releases','-f',f'tag_name={tag}','-f',f'name=Haarness {first["version"]}','-f',f'body={notes}','-F','draft=true','-F',f'prerelease={str(args.channel!="stable").lower()}')
            print(f'Created draft release {tag}')
        def check(assets):
            # GitHub computes each asset's digest on upload: the check reads GitHub's copy, not this disk.
            assets={x['name']:x for x in assets};bad=[]
            for n,(_,size,h) in files.items():
                x=assets.get(n)
                if not x or x['state']!='uploaded' or x['size']!=size or x.get('digest')!=f'sha256:{h}': bad.append(n)
            return bad,sorted(set(assets)-set(files))
        if release['draft']:
            for x in release['assets']:
                if x['name'] not in files or x['state']!='uploaded' or x.get('digest')!=f'sha256:{files[x["name"]][2]}':
                    gh('api','-X','DELETE',f'repos/{repo}/releases/assets/{x["id"]}');print(f'Removed stale asset {x["name"]}')
            have={x['name'] for x in gh('api',f'repos/{repo}/releases/{release["id"]}')['assets']}
            for n,(file,_,_) in sorted(files.items()):
                if n not in have: gh('release','upload',tag,str(file),'--repo',repo);print(f'Uploaded {n}')
            release=gh('api',f'repos/{repo}/releases/{release["id"]}')
        bad,extra=check(release['assets'])
        if bad or extra: raise ValueError(f'GitHub release {tag} differs: wrong or missing {bad}, unexpected {extra}')
        print(f'Verified on GitHub: {len(files)} assets, sizes and SHA-256 as recorded')
        if release['draft'] or (args.channel=='stable' and release['prerelease']):
            release=gh('api','-X','PATCH',f'repos/{repo}/releases/{release["id"]}','-F','draft=false','-F',f'prerelease={str(args.channel!="stable").lower()}','-f',f'make_latest={str(args.channel=="stable").lower()}')
        if not release.get('immutable'): print(f'Warning: {repo} releases are not immutable; enable release immutability in the repository settings')
        print('GitHub release:',release['html_url'])

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
    c=sub.add_parser('collect');c.add_argument('--binary',default=str(ROOT/'target/release'/('haarness.exe' if os.name=='nt' else 'haarness')));c.add_argument('--out',default=str(ROOT/'dist/releases'));c.add_argument('--allow-dirty',action='store_true');c.add_argument('--os',choices=['macos','linux','windows'],help='target OS of a cross build');c.add_argument('--arch',choices=['x64','arm64'],help='target architecture of a cross build');c.add_argument('--version',help='version of a cross-built binary that cannot run here');c.add_argument('--desktop-dir',default=str(ROOT/'apps/desktop/release'),help='where electron-builder wrote the packages');c.set_defaults(run=collect)
    c=sub.add_parser('publish');c.add_argument('--index',nargs='+',required=True);c.add_argument('--url',required=True);c.add_argument('--channel',choices=['preview','stable'],default='preview');c.add_argument('--notes-url');c.add_argument('--receipt',default='release-receipt.json');c.add_argument('--allow-dirty',action='store_true');c.add_argument('--allow-unsigned',action='store_true',help='preview only: publish a Mac Desktop that Gatekeeper rejects');c.set_defaults(run=publish)
    c=sub.add_parser('github',help='release the published artifacts on the tag\'s GitHub release');c.add_argument('--index',nargs='+',required=True);c.add_argument('--repo',default=os.environ.get('GITHUB_REPOSITORY','haarness/haarness'));c.add_argument('--channel',choices=['preview','stable'],default='preview');c.add_argument('--notes-url');c.set_defaults(run=github)
    c=sub.add_parser('collect-extension',help='package Haarness for Chrome as a zip and a signed CRX');c.add_argument('--extension-dir',default=str(ROOT/'apps/browser-extension/dist/chrome'),help='the unpacked extension: the build, or the folder `haarness browser setup` wrote from a released CLI');c.add_argument('--key',default=str(EXTENSION_KEY));c.add_argument('--version',help='refuse an extension of another version');c.add_argument('--revision',help='the commit a released CLI was built from, when packaging its folder');c.add_argument('--out',default=str(ROOT/'dist/releases/chrome-extension'));c.add_argument('--allow-dirty',action='store_true');c.set_defaults(run=collect_extension)
    c=sub.add_parser('check-macos',help='check that Gatekeeper accepts the app in each downloaded .dmg/.zip');c.add_argument('packages',nargs='+');c.set_defaults(run=check_macos)
    args=p.parse_args();args.run(args)
