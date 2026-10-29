#!/usr/bin/env python3
"""Collect real build outputs, publish immutably, and verify every downloaded byte.
No npm/Docker publication. Tokens are read from the environment, never arguments.
"""
import argparse, base64, hashlib, http.client, json, os, platform, re, shutil, subprocess, tempfile, zipfile
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

# Windows trusts a downloaded program by its Authenticode signature: unsigned, SmartScreen warns that it comes from an
# unknown publisher and Smart App Control may refuse to run it. Release builds sign every Windows program Haarness
# ships (haarness-deploy's windows-sign.sh, with Azure Artifact Signing) with an RFC 3161 timestamp from Microsoft, so
# the signatures outlive their certificates. Both chain to the root pinned beside this script.
AUTHENTICODE_ROOTS=Path(__file__).resolve().parent/'authenticode-roots.pem'
# Programs Haarness ships as their makers signed them: Bun (Oven's company) and Microsoft's DirectX DLLs in Electron
# (signed as Microsoft Windows Kits Publisher on arm64). Their signatures must be intact; a new outside signer is a
# decision, made here.
AUTHENTICODE_VENDORS={'Codeblog CORP','Microsoft Corporation','Microsoft Windows Kits Publisher'}

def authenticode(path,roots=AUTHENTICODE_ROOTS):
    # The signer's subject and the state of one Windows program's signature, from osslsigncode (whose exit status
    # ignores the timestamp): 'trusted' when the file's digest matches, the signature verifies against `roots` and the
    # timestamp against Microsoft's root; 'intact' when only the digest matches (a signer whose root is not given).
    r=subprocess.run(['osslsigncode','verify','-in',str(path),'-CAfile',str(roots),'-TSA-CAfile',str(AUTHENTICODE_ROOTS)],capture_output=True,text=True)
    lines=[l.strip() for l in (r.stdout+r.stderr).splitlines()]
    value=lambda key:next((l.split(':',1)[1].strip() for l in lines if l.startswith(key)),None)
    current,calculated=value('Current message digest'),value('Calculated message digest')
    if not current: return dict(state='unsigned')
    subject=value('Subject:') or ''
    signer=next((p[3:] for p in subject.split('/') if p.startswith('CN=')),subject)
    if current!=calculated: return dict(state='tampered',signer=signer)
    trusted=r.returncode==0 and 'Signature verification: ok' in lines and 'Timestamp Server Signature verification: ok' in lines
    return dict(state='trusted' if trusted else 'intact',signer=signer,timestamp=value('The signature is timestamped'))

def windows_programs(path):
    # (name, file) for a Windows program and every program it carries: an NSIS installer holds its uninstaller, the
    # NSIS plugins it loads while installing and the app as a 7z archive. Only files with an MZ header count: the app
    # carries Linux .node files from npm too.
    yield path.name,path
    if not path.name.startswith('Haarness-'): return
    with tempfile.TemporaryDirectory(prefix='haarness-authenticode-') as d:
        subprocess.run(['7zz','x','-bd','-y',f'-o{d}/installer',str(path)],check=True,stdout=subprocess.DEVNULL)
        apps=sorted(Path(d,'installer','$PLUGINSDIR').glob('app-*.7z'))
        if len(apps)!=1: raise ValueError(f'{path.name}: expected one app archive in the installer, found {apps}')
        subprocess.run(['7zz','x','-bd','-y',f'-o{d}/app',str(apps[0])],check=True,stdout=subprocess.DEVNULL)
        for f in sorted(Path(d).rglob('*')):
            if f.is_file() and f.suffix.lower() in ('.exe','.dll','.node') and f.open('rb').read(2)==b'MZ':
                yield f'{path.name}/{f.relative_to(d).as_posix()}',f

def check_windows(args):
    # Every Windows program in the given CLIs and installers signed by --publisher, its signature and timestamp
    # trusted, or signed by one of AUTHENTICODE_VENDORS and intact. --test-ca trusts a throwaway root instead of the
    # pinned one (to prove the pipeline); the record then says publicTrust false and publish treats it as unsigned.
    roots=Path(args.test_ca) if args.test_ca else AUTHENTICODE_ROOTS
    record=dict(publisher=args.publisher,publicTrust=not args.test_ca,files={});bad=[]
    for p in args.files:
        path=Path(p);programs=[]
        for name,f in windows_programs(path):
            a=authenticode(f,roots);a['name']=name;programs.append(a)
            ours=a['state']=='trusted' and a['signer']==args.publisher
            vendor=a['state'] in ('trusted','intact') and a['signer'] in AUTHENTICODE_VENDORS
            if not (ours or vendor): bad.append(f'{name}: {a["state"]}'+(f', signed by {a["signer"]}' if a.get('signer') else ''))
            print(f'{name}: {a["state"]}'+(f', {a["signer"]}' if a.get('signer') else ''))
        record['files'][digest(path)]=dict(file=path.name,signer=programs[0].get('signer'),programs=programs)
    if bad: raise ValueError('Windows programs not signed as a release needs:\n  '+'\n  '.join(bad))
    if args.record: Path(args.record).write_text(json.dumps(record,indent=2)+'\n')
    print(f'PASS {len(args.files)} Windows files, every program in them signed by {args.publisher} or a vendor'+('' if record['publicTrust'] else ' (test root, not publicly trusted)'))

def workspace_version(cargo_toml):
    # [workspace.package] version of a Cargo workspace (Python 3.10 on the build machines has no tomllib).
    section=None
    for line in Path(cargo_toml).read_text().splitlines():
        line=line.strip()
        if line.startswith('['): section=line
        elif section=='[workspace.package]' and (m:=re.fullmatch(r'version\s*=\s*"([^"]+)"',line)): return m[1]
    raise ValueError(f'{cargo_toml}: no [workspace.package] version')

def router_build(args, version):
    # Haarness Router ships with every release, at the release's version, built from its own repository: its commit,
    # whether that checkout was clean, and its version (from the checkout, so a cross build that cannot run is checked
    # too; one that runs here must also report it).
    source=Path(args.router_source).resolve()
    revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=source,text=True).strip()
    dirty=subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=source,text=True).strip()
    if dirty and not args.allow_dirty: raise ValueError(f'Commit the router sources in {source} before collecting a publishable release (or --allow-dirty for local proof).')
    if (found:=workspace_version(source/'Cargo.toml'))!=version: raise ValueError(f'The router in {source} is version {found}, not {version}: bump its workspace version and tag it v{version}')
    if not args.version:
        reported=subprocess.check_output([str(Path(args.router_binary).resolve()),'--version'],text=True).strip()
        if reported!=f'haarness-router {version}': raise ValueError(f'The router binary reports {reported!r}, not haarness-router {version}')
    return revision,bool(dirty)

def collect(args):
    revision = subprocess.check_output(['git','rev-parse','HEAD'], cwd=ROOT, text=True).strip()
    dirty = subprocess.check_output(['git','status','--porcelain','--untracked-files=no'], cwd=ROOT, text=True).strip()
    if dirty and not args.allow_dirty: raise ValueError('Commit build sources before collecting a publishable release (or --allow-dirty for local proof).')
    # The build host's platform, unless --os/--arch name the target of a cross build (Linux arm64 or Windows from
    # a Linux x64 host when CI is unavailable). A cross-built binary cannot run here: --version names it then.
    os_name = args.os or {'Darwin':'macos','Linux':'linux','Windows':'windows'}[platform.system()]
    # Windows reports ARM64 in capitals (and AMD64), macOS arm64, Linux aarch64.
    arch = args.arch or {'arm64':'arm64','aarch64':'arm64','amd64':'x64','x86_64':'x64'}[platform.machine().lower()]
    if args.router_only and not args.router_binary: raise ValueError('--router-only collects the --router-binary')
    binary = Path(args.binary).resolve()
    version = args.version or (subprocess.check_output([str(Path(args.router_binary).resolve()),'--version'],text=True).strip().removeprefix('haarness-router ') if args.router_only
                               else subprocess.check_output([str(binary),'--version'],text=True).strip().removeprefix('haarness '))
    # A router-only index adds the router to a release already published from the same commit (the service adds a
    # product to the channel's current release); its CLI and Desktop are that release's.
    if not args.router_only:
        desktop = json.loads((ROOT/'apps/desktop/package.json').read_text())
        if desktop['version'] != version: raise ValueError('CLI and Desktop versions differ')
    router_revision,router_dirty = router_build(args,version) if args.router_binary else (None,False)
    out = Path(args.out).resolve(); out.mkdir(parents=True, exist_ok=True)
    artifacts = []
    # check-windows' record of the Windows files (--authenticode), by SHA-256.
    record=json.loads(Path(args.authenticode).read_text()) if args.authenticode else {}
    authenticode_record=record.get('files',{});authenticode_publicly_trusted=bool(record.get('publicTrust'))
    def add(path, product, kind, name=None, **fields):
        name = name or path.name
        dest = out/name
        if path.resolve() != dest: shutil.copy2(path,dest)
        artifact=dict(file=name, product=product, kind=kind, os=os_name, arch=arch, size=dest.stat().st_size, sha256=digest(dest), **fields)
        # Recorded for a publisher that is not a Mac (CI publishes from Linux); publish refuses a package without it.
        if os_name=='macos' and product=='desktop':
            try: artifact['gatekeeper']=gatekeeper(dest)
            except ValueError as e: print(f'Warning, publish will refuse this package: {e}')
        # check-windows' record of the very same bytes, made where osslsigncode and 7-Zip are (the build container).
        if os_name=='windows':
            checked=authenticode_record.get(artifact['sha256'])
            if checked: artifact['authenticode']=dict(signer=checked['signer'],publicTrust=authenticode_publicly_trusted,programs=len(checked['programs']))
            else: print(f'Warning, publish will refuse {name} without --allow-unsigned-windows: no check-windows record of it')
        artifacts.append(artifact)
    exe='.exe' if os_name=='windows' else ''
    if not args.router_only:
        add(binary,'cli','binary',f'haarness-{os_name}-{arch}{exe}')
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
    # The router's own commit beside the release's: its GitHub release is on the router repository's tag.
    if args.router_binary: add(Path(args.router_binary).resolve(),'router','binary',f'haarness-router-{os_name}-{arch}{exe}',sourceRevision=router_revision)
    index = dict(version=version, sourceRevision=revision, dirty=bool(dirty) or router_dirty, releasedAt=datetime.now(timezone.utc).isoformat(), artifacts=artifacts)
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
    if args.allow_unsigned_windows: print('Warning: publishing Windows files without a publicly trusted signature (SmartScreen warns, Smart App Control may block)')
    indexes=[(Path(p),json.loads(Path(p).read_text())) for p in args.index]
    first=indexes[0][1]; ids=[]; seen=set(); uploads=[]
    # Haarness Router ships with every platform's CLI, all from one router commit.
    built=lambda product:{(a['os'],a['arch']) for _,index in indexes for a in index['artifacts'] if a['product']==product}
    if (missing:=sorted(built('cli')-built('router'))) and not args.without_router:
        raise ValueError(f'No Haarness Router for {", ".join("-".join(p) for p in missing)}: collect --router-binary on every platform (or --without-router)')
    if len({a.get('sourceRevision') for _,index in indexes for a in index['artifacts'] if a['product']=='router'}-{None})>1 or \
            any(a['product']=='router' and not a.get('sourceRevision') for _,index in indexes for a in index['artifacts']):
        raise ValueError('Every router build must record the one router commit it was built from')
    # Files without a CLI (Haarness for Chrome or the router, added later) can only join the channel's current release
    # of the same version and commit; on their own they would make a release that installs no CLI.
    if not built('cli'):
        try: current=request(args.url,'GET','/api/v1/releases?'+urlencode({'channel':args.channel}))['manifest']
        except RuntimeError as e:
            if ': 404 ' not in str(e): raise
            current={}
        if (current.get('version'),current.get('source_revision'))!=(first['version'],first['sourceRevision']):
            raise ValueError(f'These files carry no CLI, so they can only join the {args.channel} channel\'s current release of '
                             f'{first["version"]} from {first["sourceRevision"]}, which is not there')
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
            # Windows: collect's check-windows record of a publicly trusted signature, the signer checked again on the
            # very bytes where osslsigncode is installed. Unsigned (or test-signed) is the owner's explicit decision.
            if a['os']=='windows' and not (args.allow_unsigned or args.allow_unsigned_windows):
                signed=a.get('authenticode') or {}
                if not signed.get('publicTrust'): raise ValueError(f'{a["file"]}: no record of a publicly trusted Authenticode signature; sign the Windows build, or pass --allow-unsigned-windows')
                if shutil.which('osslsigncode'):
                    now=authenticode(file)
                    if now['state']!='trusted' or now['signer']!=signed['signer']: raise ValueError(f'{a["file"]}: Authenticode {now}, recorded {signed}')
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
    # Mirrors a publication onto GitHub releases, with the bytes the indexes record and a SHA256SUMS file: the CLI,
    # Desktop and Haarness for Chrome on this repository's tag, Haarness Router on the router repository's tag of the
    # same version (the commit its builds record). Each release stays a draft until GitHub reports every asset
    # uploaded with the expected size and SHA-256, so it never shows a partial set; a published release is never
    # changed, only checked (and a preview made stable).
    indexes=[(Path(p),json.loads(Path(p).read_text())) for p in args.index]
    first=indexes[0][1];groups={'haarness':{},'router':{}};router_revision=None
    for path,index in indexes:
        if index.get('dirty'): raise ValueError('Dirty artifacts are not released on GitHub')
        if any(index[k]!=first[k] for k in ('version','sourceRevision')): raise ValueError('All builds must share version and source commit')
        for a in index['artifacts']:
            file=path.parent/a['file']
            if file.parent.resolve()!=path.parent.resolve() or digest(file)!=a['sha256'] or file.stat().st_size!=a['size']: raise ValueError(f'Artifact changed: {file}')
            files=groups['router' if a['product']=='router' else 'haarness']
            if a['file'] in files or a['file']=='SHA256SUMS': raise ValueError(f'Duplicate asset name: {a["file"]}')
            files[a['file']]=(file,a['size'],a['sha256'])
            if a['product']=='router':
                if router_revision not in (None,a.get('sourceRevision')) or not a.get('sourceRevision'): raise ValueError('Every router build must record the one router commit it was built from')
                router_revision=a['sourceRevision']
    version=first['version']
    releases={
        'haarness':(args.repo,first['sourceRevision'],f'Haarness {version}',
                    f'Haarness {version}, built from {first["sourceRevision"]}. The same files as https://haarness.ai/download; SHA-256 sums in SHA256SUMS.'),
        'router':(args.router_repo,router_revision,f'Haarness Router {version}',
                  f'Haarness Router {version}, built from {router_revision}, released with Haarness {version}. Install it with '
                  '`curl -fsSL https://haarness.ai/install-router.sh | sh` (macOS, Linux) or `irm https://haarness.ai/install-router.ps1 | iex` '
                  '(Windows); SHA-256 sums in SHA256SUMS.')}
    for name in [args.only] if args.only else ['haarness','router']:
        if not groups[name]:
            if args.only: raise ValueError(f'No {name} files in these indexes')
            continue
        repo,revision,title,notes=releases[name]
        mirror(repo,f'v{version}',revision,title,notes+(f'\n\n{args.notes_url}' if args.notes_url else ''),dict(groups[name]),args.channel)

def mirror(repo,tag,revision,title,notes,files,channel):
    # One GitHub release: `files` ({name: (path, size, sha256)}) and their SHA256SUMS on `repo`'s `tag`, which must be
    # `revision`.
    if gh('api',f'repos/{repo}/commits/{tag}')['sha']!=revision: raise ValueError(f'{repo} {tag} is not {revision}')
    with tempfile.TemporaryDirectory(prefix='haarness-github-') as d:
        sums=Path(d)/'SHA256SUMS';sums.write_text(''.join(f'{h}  {n}\n' for n,(_,_,h) in sorted(files.items())))
        files['SHA256SUMS']=(sums,sums.stat().st_size,digest(sums))
        found=[r for page in gh('api','--paginate','--slurp',f'repos/{repo}/releases') for r in page if r['tag_name']==tag]
        if len(found)>1: raise ValueError(f'{repo} has {len(found)} releases for {tag}')
        release=found[0] if found else None
        if release is None:
            release=gh('api','-X','POST',f'repos/{repo}/releases','-f',f'tag_name={tag}','-f',f'name={title}','-f',f'body={notes}','-F','draft=true','-F',f'prerelease={str(channel!="stable").lower()}')
            print(f'Created draft release {repo} {tag}')
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
        if bad or extra: raise ValueError(f'GitHub release {repo} {tag} differs: wrong or missing {bad}, unexpected {extra}')
        print(f'Verified on GitHub: {repo} {tag}, {len(files)} assets, sizes and SHA-256 as recorded')
        if release['draft'] or (channel=='stable' and release['prerelease']):
            release=gh('api','-X','PATCH',f'repos/{repo}/releases/{release["id"]}','-F','draft=false','-F',f'prerelease={str(channel!="stable").lower()}','-f',f'make_latest={str(channel=="stable").lower()}')
        if not release.get('immutable'): print(f'Warning: {repo} releases are not immutable; enable release immutability in the repository settings')
        print('GitHub release:',release['html_url'])

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
    c=sub.add_parser('collect');c.add_argument('--binary',default=str(ROOT/'target/release'/('haarness.exe' if os.name=='nt' else 'haarness')));c.add_argument('--out',default=str(ROOT/'dist/releases'));c.add_argument('--allow-dirty',action='store_true');c.add_argument('--os',choices=['macos','linux','windows'],help='target OS of a cross build');c.add_argument('--arch',choices=['x64','arm64'],help='target architecture of a cross build');c.add_argument('--version',help='version of a cross-built binary that cannot run here');c.add_argument('--desktop-dir',default=str(ROOT/'apps/desktop/release'),help='where electron-builder wrote the packages');c.add_argument('--authenticode',help="check-windows' record of the Windows files");c.add_argument('--router-binary',help='the Haarness Router binary built for this platform from --router-source');c.add_argument('--router-source',default=str(ROOT.parent/'haarness-router'),help='the clean haarness-router checkout (at its v<version> tag) the router was built from');c.add_argument('--router-only',action='store_true',help='collect only the router, to add it to a release already published from this commit');c.set_defaults(run=collect)
    c=sub.add_parser('publish');c.add_argument('--index',nargs='+',required=True);c.add_argument('--url',required=True);c.add_argument('--channel',choices=['preview','stable'],default='preview');c.add_argument('--notes-url');c.add_argument('--receipt',default='release-receipt.json');c.add_argument('--allow-dirty',action='store_true');c.add_argument('--allow-unsigned',action='store_true',help='preview only: publish a Mac Desktop that Gatekeeper rejects');c.add_argument('--allow-unsigned-windows',action='store_true',help='publish Windows files without a publicly trusted Authenticode signature (the owner\'s decision)');c.add_argument('--without-router',action='store_true',help='publish CLI builds that carry no Haarness Router (local proof only)');c.set_defaults(run=publish)
    c=sub.add_parser('github',help='release the published artifacts on the tag\'s GitHub release');c.add_argument('--index',nargs='+',required=True);c.add_argument('--repo',default=os.environ.get('GITHUB_REPOSITORY','haarness/haarness'));c.add_argument('--router-repo',default='haarness/haarness-router');c.add_argument('--only',choices=['haarness','router'],help="mirror only this repository's release");c.add_argument('--channel',choices=['preview','stable'],default='preview');c.add_argument('--notes-url');c.set_defaults(run=github)
    c=sub.add_parser('collect-extension',help='package Haarness for Chrome as a zip and a signed CRX');c.add_argument('--extension-dir',default=str(ROOT/'apps/browser-extension/dist/chrome'),help='the unpacked extension: the build, or the folder `haarness browser setup` wrote from a released CLI');c.add_argument('--key',default=str(EXTENSION_KEY));c.add_argument('--version',help='refuse an extension of another version');c.add_argument('--revision',help='the commit a released CLI was built from, when packaging its folder');c.add_argument('--out',default=str(ROOT/'dist/releases/chrome-extension'));c.add_argument('--allow-dirty',action='store_true');c.set_defaults(run=collect_extension)
    c=sub.add_parser('check-macos',help='check that Gatekeeper accepts the app in each downloaded .dmg/.zip');c.add_argument('packages',nargs='+');c.set_defaults(run=check_macos)
    c=sub.add_parser('check-windows',help='check the Authenticode signature of every Windows program in each CLI and installer (needs osslsigncode and 7zz)');c.add_argument('files',nargs='+');c.add_argument('--publisher',required=True,help="the signing certificate's subject CN");c.add_argument('--test-ca',help='trust this throwaway root instead of the pinned one');c.add_argument('--record',help='write what was checked here, for collect --authenticode');c.set_defaults(run=check_windows)
    args=p.parse_args();args.run(args)
