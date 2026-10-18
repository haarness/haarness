#!/usr/bin/env python3
"""Collect real build outputs, publish immutably, and verify every downloaded byte.
No npm/Docker publication. Tokens are read from the environment, never arguments.
"""
import argparse, hashlib, http.client, json, os, platform, shutil, subprocess
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit, urlencode
ROOT = Path(__file__).resolve().parents[1]

def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024*1024), b''): h.update(chunk)
    return h.hexdigest()

def collect(args):
    revision = subprocess.check_output(['git','rev-parse','HEAD'], cwd=ROOT, text=True).strip()
    dirty = subprocess.check_output(['git','status','--porcelain','--untracked-files=no'], cwd=ROOT, text=True).strip()
    if dirty and not args.allow_dirty: raise ValueError('Commit build sources before collecting a publishable release (or --allow-dirty for local proof).')
    os_name = {'Darwin':'macos','Linux':'linux','Windows':'windows'}[platform.system()]
    arch = {'arm64':'arm64','aarch64':'arm64','AMD64':'x64','x86_64':'x64'}[platform.machine()]
    binary = Path(args.binary).resolve()
    version = subprocess.check_output([str(binary),'--version'],text=True).strip().removeprefix('haarness ')
    desktop = json.loads((ROOT/'apps/desktop/package.json').read_text())
    if desktop['version'] != version: raise ValueError('CLI and Desktop versions differ')
    out = Path(args.out).resolve(); out.mkdir(parents=True, exist_ok=True)
    artifacts = []
    def add(path, product, kind, name=None):
        name = name or path.name
        dest = out/name
        if path.resolve() != dest: shutil.copy2(path,dest)
        artifacts.append(dict(file=name, product=product, kind=kind, os=os_name, arch=arch, size=dest.stat().st_size, sha256=digest(dest)))
    add(binary,'cli','binary',f'haarness-{os_name}-{arch}'+('.exe' if os_name=='windows' else ''))
    extensions = {'macos':{'dmg','zip'},'linux':{'AppImage','deb'},'windows':{'exe'}}[os_name]
    build_os = {'macos':'mac','linux':'linux','windows':'win'}[os_name]
    # electron-builder names Linux packages by each format's word for the CPU
    # (x86_64 for AppImage, amd64 for .deb); publish every package as x64/arm64.
    arch_names = {'x64':{'x64','x86_64','amd64'},'arm64':{'arm64','aarch64'}}[arch]
    for path in sorted((ROOT/'apps/desktop/release').iterdir()):
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
    indexes=[(Path(p),json.loads(Path(p).read_text())) for p in args.index]
    first=indexes[0][1]; ids=[]; seen=set()
    for path,index in indexes:
        if index.get('dirty') and not (args.allow_dirty and urlsplit(args.url).hostname in ('127.0.0.1','localhost')): raise ValueError('Dirty artifacts can only be used for explicit local proof')
        if any(index[k]!=first[k] for k in ('version','sourceRevision')): raise ValueError('All builds must share version and source commit')
        for a in index['artifacts']:
            file=path.parent/a['file']
            if file.parent.resolve()!=path.parent.resolve() or digest(file)!=a['sha256'] or file.stat().st_size!=a['size']: raise ValueError(f'Artifact changed: {file}')
            identity=(a['product'],a['os'],a['arch'],a['kind'])
            if identity in seen: raise ValueError(f'Duplicate artifact: {identity}')
            seen.add(identity)
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
    c=sub.add_parser('collect');c.add_argument('--binary',default=str(ROOT/'target/release'/('haarness.exe' if os.name=='nt' else 'haarness')));c.add_argument('--out',default=str(ROOT/'dist/releases'));c.add_argument('--allow-dirty',action='store_true');c.set_defaults(run=collect)
    c=sub.add_parser('publish');c.add_argument('--index',nargs='+',required=True);c.add_argument('--url',required=True);c.add_argument('--channel',choices=['preview','stable'],default='preview');c.add_argument('--notes-url');c.add_argument('--receipt',default='release-receipt.json');c.add_argument('--allow-dirty',action='store_true');c.set_defaults(run=publish)
    args=p.parse_args();args.run(args)
