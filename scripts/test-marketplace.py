#!/usr/bin/env python3
"""Verified catalog -> CLI/API/tools -> live runtime lifecycle using owned fixtures.

Runs actual Bun/npm, ZIP installs and hot reload, without publishing packages or
using user configuration. Public provider/Chrome proof lives in Cloud verification.
"""
import base64
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tarfile
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()


def zip_bytes(files):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, text in files.items(): archive.writestr(name, text)
    return buf.getvalue()


def main():
    with tempfile.TemporaryDirectory(prefix="haarness-marketplace-e2e-") as temp:
        root = Path(temp).resolve()
        workspace, home, data = (root / n for n in ("workspace", "home", "data"))
        for p in (workspace, home, data): p.mkdir()
        env = {k:v for k,v in os.environ.items() if k in ("PATH", "TMPDIR", "LANG", "TERM", "HAARNESS_BUN")}
        env.update(HOME=str(home), HAARNESS_TEST_PROVIDER="1", RUST_LOG="warn")
        resources, manifests, artifacts = {}, {}, {}
        files = {
            "skill": {"SKILL.md": "---\nname: proof-skill\ndescription: marketplace skill proof\n---\nRead references/proof.txt.\n", "references/proof.txt": "MARKETPLACE_SUPPORT_FILE"},
            "agent": {"proof-agent.md": "---\ndescription: marketplace verifier\nmode: all\n---\nVerify marketplace proof."},
            "plugin": {".claude-plugin/plugin.json": '{"name":"proof-plugin"}', "skills/plugin-proof/SKILL.md": "---\nname: plugin-proof\ndescription: Plugin proof\n---\nPLUGIN_PROOF"},
        }
        package = {"name":"marketplace-fixture", "version":"1.0.0", "type":"module", "haarness":{"extension":"index.ts"}, "scripts":{"postinstall":f"touch {root / 'POSTINSTALL_EXECUTED'}"}}
        source = f"import {{writeFileSync}} from 'node:fs';writeFileSync({json.dumps(str(root / 'INITIALIZED'))},'yes');export default {{apiVersion:1,name:'marketplace_fixture',tools:{{ping:{{description:'marketplace proof',parameters:{{type:'object'}},execute(){{return 'MARKETPLACE_EXTENSION_OK'}}}}}}}};"
        buf=io.BytesIO()
        with tarfile.open(fileobj=buf,mode="w:gz") as archive:
            for name,body in {"package.json":json.dumps(package),"index.ts":source}.items():
                entry=tarfile.TarInfo("package/"+name);content=body.encode();entry.size=len(content);archive.addfile(entry,io.BytesIO(content))
        tgz=buf.getvalue();integrity="sha512-"+base64.b64encode(hashlib.sha512(tgz).digest()).decode()

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_GET(self):
                parsed=urllib.parse.urlsplit(self.path); path=parsed.path; query=urllib.parse.parse_qs(parsed.query)
                if path.endswith(".tgz"): payload=tgz
                elif path.startswith("/marketplace-fixture"):
                    version={**package,"dist":{"tarball":base+"/marketplace-fixture/-/fixture.tgz","integrity":integrity}}
                    payload={"name":package["name"],"dist-tags":{"latest":"1.0.0"},"versions":{"1.0.0":version}}
                elif path.startswith("/artifacts/"): payload=artifacts[path.rsplit("/",1)[-1]]
                elif path.endswith("/collections"): payload={"items":[{"id":"fixture","slug":"fixtures","name":"Fixtures"}],"nextCursor":None}
                elif path.endswith("/resources"):
                    items=[r for r in resources.values() if (not query.get("kind") or r["kind"]==query["kind"][0]) and (not query.get("q") or query["q"][0] in r["slug"])]
                    payload={"items":items,"nextCursor":None}
                else:
                    rid=path.split("/resources/")[-1].split("/")[0]
                    if rid not in resources: self.send_error(404);return
                    if path.endswith("/install-manifest"): payload=manifests[rid]
                    elif path.endswith("/versions"): payload={"items":[manifests[rid]["version"]]}
                    else: payload={**resources[rid],"versions":[manifests[rid]["version"]]}
                body=payload if isinstance(payload,bytes) else json.dumps(payload).encode()
                self.send_response(200);self.send_header("content-type","application/octet-stream" if isinstance(payload,bytes) else "application/json");self.send_header("content-length",str(len(body)));self.end_headers();self.wfile.write(body)

        catalog=http.server.ThreadingHTTPServer(("127.0.0.1",0),Handler)
        base=f"http://127.0.0.1:{catalog.server_port}"
        threading.Thread(target=catalog.serve_forever,daemon=True).start()
        env["HAARNESS_CLOUD_API_URL"]=base
        (home/".npmrc").write_text("registry="+base+"\n")
        ids={}
        for kind in ("skill","agent","plugin","mcp","extension"):
            rid,vid=str(uuid.uuid4()),str(uuid.uuid4());ids[kind]=rid;name="proof-"+kind
            resource={"id":rid,"kind":kind,"slug":name,"name":name,"summary":"Fixture","visibility":"public","publisher":{"slug":"fixtures","displayName":"Fixtures"}}
            install={kind:{}}
            if kind=="mcp": install={"mcp":{"config":{"type":"remote","url":"https://learn.microsoft.com/api/mcp"},"env_placeholders":[]}}
            if kind=="extension": install={"extension":{"extension_type":"core","npm_spec":"marketplace-fixture@1.0.0","integrity":integrity}}
            version={"id":vid,"number":1,"status":"approved","yanked":False}
            manifest={"manifestVersion":1,"resource":resource,"version":version,"compatibility":{},"capabilities":{},"install":install,"lock":{"resourceId":rid,"versionId":vid}}
            if kind in files:
                body=zip_bytes(files[kind]);artifacts[rid]=body;manifest["artifact"]={"url":base+"/artifacts/"+rid,"sha256":hashlib.sha256(body).hexdigest(),"sizeBytes":len(body)}
            resources[rid]=resource;manifests[rid]=manifest
        prefix=[str(BIN),"--workspace",str(workspace),"--data-dir",str(data),"--trust-project"]
        def run(*args,ok=True,trust=True):
            selected_prefix=prefix if trust else [*(arg for arg in prefix if arg != "--trust-project"), "--disable-project-config"]
            result=subprocess.run([*selected_prefix,*args],env=env,capture_output=True,text=True,timeout=150)
            if ok: assert result.returncode==0,(args,result.stdout,result.stderr)
            return result
        def marketplace(action,**args): return json.loads(run("marketplace","request",json.dumps({"action":action,**args})).stdout)
        def wait(fn,label):
            for _ in range(150):
                try:
                    if fn(): return
                except (OSError,ValueError): pass
                time.sleep(.2)
            raise AssertionError(label)
        process=None
        try:
            assert len(marketplace("search")["items"])==5
            assert marketplace("collections")["items"]
            assert marketplace("detail",id="fixtures/proof-skill",kind="skill")["id"]==ids["skill"]
            assert marketplace("versions",id=ids["skill"])["items"]
            # A global install must not execute definitions from an untrusted checkout.
            untrusted=workspace/".haarness/extensions/untrusted.ts"
            untrusted.parent.mkdir(parents=True)
            untrusted.write_text("throw new Error('UNTRUSTED_PROJECT_EXTENSION_EXECUTED');")
            (workspace/".haarness/extensions.json").write_text('{"enabled":true,"entries":[]}')
            run("marketplace","request",json.dumps({"action":"install","id":ids["extension"],"scope":"global"}),trust=False)
            run("marketplace","request",json.dumps({"action":"remove","id":ids["extension"],"scope":"global"}),trust=False)
            untrusted.unlink()
            (workspace/".haarness/extensions.json").unlink()
            (root/"INITIALIZED").unlink()
            for kind in ("skill","agent","plugin","mcp"):
                marketplace("install",kind=kind,id=ids[kind],scope="local")
            assert (workspace/".haarness/skills/proof-skill/references/proof.txt").read_text()=="MARKETPLACE_SUPPORT_FILE"
            bad=json.loads(json.dumps(manifests[ids["extension"]]));bad["install"]["extension"]["integrity"]="sha512-invalid"
            result=run("marketplace","request",json.dumps({"action":"install","manifest":bad,"scope":"local"}),ok=False)
            assert result.returncode and "integrity" in result.stderr,result.stderr
            assert not (root/"INITIALIZED").exists(),"Mismatched package executed initializer"
            extension=marketplace("install",id=ids["extension"],scope="local")
            assert extension["name"]=="marketplace_fixture"
            assert (root/"INITIALIZED").exists() and not (root/"POSTINSTALL_EXECUTED").exists()
            sock=socket.socket();sock.bind(("127.0.0.1",0));port=sock.getsockname()[1];sock.close()
            token="marketplace-e2e-token";url=f"http://127.0.0.1:{port}"
            log=open(root/"server.log","w")
            process=subprocess.Popen([*prefix,"serve","--host","127.0.0.1","--port",str(port),"--token",token],env=env,stdout=log,stderr=log)
            def api(path,body=None):
                request=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={"Authorization":"Bearer "+token,"Content-Type":"application/json"})
                with urllib.request.urlopen(request,timeout=15) as response:return json.load(response)
            wait(lambda: api("/v1/info"),"server start")
            def has(path,key,name):return any(x.get(key)==name for x in api(path))
            wait(lambda:has("/v1/skills","name","proof-skill"),"skill reload")
            assert has("/v1/agents","id","proof-agent")
            assert has("/v1/skills","name","proof-plugin:plugin-proof")
            result=run("run","--attach",url,"--token",token,"--model","echo/echo-1","!tool ext__marketplace_fixture__ping {}")
            assert "MARKETPLACE_EXTENSION_OK" in result.stdout,result.stdout
            for kind in ("skill","agent","plugin","mcp","extension"):
                api("/v1/manage/marketplace",{"action":"disable","id":ids[kind],"scope":"local"})
            wait(lambda:not has("/v1/skills","name","proof-skill") and not has("/v1/agents","id","proof-agent") and not has("/v1/skills","name","proof-plugin:plugin-proof"),"disabled resources leave runtime")
            for kind in ids: marketplace("update",id=ids[kind],scope="local")
            assert api("/v1/manage/capabilities",{"action":"list"})["extensions"][0]["enabled"] is False
            process.terminate();process.wait(timeout=20);process=None
            process=subprocess.Popen([*prefix,"serve","--host","127.0.0.1","--port",str(port),"--token",token],env=env,stdout=log,stderr=log)
            wait(lambda:api("/v1/info"),"restart")
            assert not has("/v1/skills","name","proof-skill") and not has("/v1/agents","id","proof-agent")
            for kind in ids: api("/v1/manage/marketplace",{"action":"enable","id":ids[kind],"scope":"local"})
            wait(lambda:has("/v1/skills","name","proof-skill") and has("/v1/agents","id","proof-agent"),"reenable")
            for kind in ids: api("/v1/manage/marketplace",{"action":"remove","id":ids[kind],"scope":"local"})
            wait(lambda:not has("/v1/skills","name","proof-skill"),"remove")
            assert marketplace("installed")==[]
            assert not (workspace/".haarness/plugins/proof-plugin").exists()
            # Uninstalling an extension deletes the package it installed, and updates replaced theirs.
            leftovers=[p for p in data.rglob("extensions/packages/*") if p.parent.name=="packages"]
            assert not leftovers,leftovers
            print("PASS all five kinds: catalog/collections/detail/versions, verified install, Bun integrity-before-execution, executable tool, live disable/update/enable/remove, restart persistence, no package left behind")
        finally:
            if process: process.terminate();process.wait(timeout=20)
            catalog.shutdown()
            run("serve","--stop",ok=False)

if __name__=="__main__": main()
