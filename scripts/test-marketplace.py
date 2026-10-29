#!/usr/bin/env python3
"""Verified catalog -> CLI/API/tools -> live runtime lifecycle using owned fixtures.

Runs actual Bun/npm, ZIP installs and hot reload, without publishing packages or
using user configuration. Public provider/Chrome proof lives in Cloud verification.
Then automatic updates: new versions published in the fixture catalog reach installs
that follow them, through `check-updates` and through a server's background round
(HAARNESS_AUTO_UPDATE_INTERVAL_SECS=60, so that part waits about a minute), while a
pinned install stays where it is.
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
from e2e_env import ISOLATED

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
        env.update(HOME=str(home), HAARNESS_TEST_PROVIDER="1", RUST_LOG="warn", **ISOLATED)
        resources, manifests, artifacts, history = {}, {}, {}, {}
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
                    if path.endswith("/install-manifest"):
                        payload=history[rid][int(query["version"][0])] if query.get("version") else manifests[rid]
                    elif path.endswith("/versions"): payload={"items":[manifests[rid]["version"]]}
                    else: payload={**resources[rid],"versions":[manifests[rid]["version"]],"activeVersion":manifests[rid]["version"]}
                body=payload if isinstance(payload,bytes) else json.dumps(payload).encode()
                self.send_response(200);self.send_header("content-type","application/octet-stream" if isinstance(payload,bytes) else "application/json");self.send_header("content-length",str(len(body)));self.end_headers();self.wfile.write(body)

        catalog=http.server.ThreadingHTTPServer(("127.0.0.1",0),Handler)
        base=f"http://127.0.0.1:{catalog.server_port}"
        threading.Thread(target=catalog.serve_forever,daemon=True).start()
        env["HAARNESS_CLOUD_API_URL"]=base
        # Cloud's public host, where resource icons live; only icons there are passed on.
        icons="https://cloud.fixture.test/icons"
        env["HAARNESS_CLOUD_WEB_ORIGIN"]="https://cloud.fixture.test"
        (home/".npmrc").write_text("registry="+base+"\n")
        ids={}
        for kind in ("skill","agent","plugin","mcp","extension"):
            rid,vid=str(uuid.uuid4()),str(uuid.uuid4());ids[kind]=rid;name="proof-"+kind
            resource={"id":rid,"kind":kind,"slug":name,"name":name,"summary":"Fixture","visibility":"public","publisher":{"slug":"fixtures","displayName":"Fixtures"},
                      "iconUrl":"https://elsewhere.example/agent.png" if kind=="agent" else f"{icons}/{kind}.png"}
            install={kind:{}}
            if kind=="mcp": install={"mcp":{"config":{"type":"remote","url":"https://learn.microsoft.com/api/mcp"},"env_placeholders":[]}}
            if kind=="extension": install={"extension":{"extension_type":"core","npm_spec":"marketplace-fixture@1.0.0","integrity":integrity}}
            version={"id":vid,"number":1,"status":"approved","yanked":False}
            manifest={"manifestVersion":1,"resource":resource,"version":version,"compatibility":{},"capabilities":{},"install":install,"lock":{"resourceId":rid,"versionId":vid}}
            if kind in files:
                body=zip_bytes(files[kind]);artifacts[rid]=body;manifest["artifact"]={"url":base+"/artifacts/"+rid,"sha256":hashlib.sha256(body).hexdigest(),"sizeBytes":len(body)}
            resources[rid]=resource;manifests[rid]=manifest;history[rid]={1:manifest}
        def publish_skill(slug,number,text,rid=None):
            """Version `number` of the skill `slug`, now the active one; a new resource without `rid`."""
            rid=rid or str(uuid.uuid4());vid=str(uuid.uuid4())
            resource={"id":rid,"kind":"skill","slug":slug,"name":slug,"summary":"Fixture","visibility":"public","publisher":{"slug":"fixtures","displayName":"Fixtures"}}
            body=zip_bytes({"SKILL.md":f"---\nname: {slug}\ndescription: automatic update proof\n---\n{text}\n"});artifacts[vid]=body
            version={"id":vid,"number":number,"status":"approved","yanked":False}
            manifest={"manifestVersion":1,"resource":resource,"version":version,"compatibility":{},"capabilities":{},"install":{"skill":{}},"lock":{"resourceId":rid,"versionId":vid},
                      "artifact":{"url":base+"/artifacts/"+vid,"sha256":hashlib.sha256(body).hexdigest(),"sizeBytes":len(body)}}
            resources[rid]=resource;manifests[rid]=manifest;history.setdefault(rid,{})[number]=manifest
            return rid
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
            # Icons on Cloud reach the search rows, the install form, the receipts and the
            # installed list; the one on another host never does.
            expected={f"proof-{kind}":None if kind=="agent" else f"{icons}/{kind}.png" for kind in ids}
            rows=api("/v1/manage/marketplace-ui",{"action":"search"})["entries"]
            assert {e["label"].split(" · ")[0]:e.get("icon") for e in rows if e["request"].get("action")=="inspect"}==expected,rows
            assert api("/v1/manage/marketplace-ui",{"action":"inspect","id":ids["skill"]})["icon"]==f"{icons}/skill.png"
            assert "icon" not in api("/v1/manage/marketplace-ui",{"action":"inspect","id":ids["agent"]})
            receipts=marketplace("installed")
            assert {f"proof-{r['kind']}":r.get("iconUrl") for r in receipts}==expected,receipts
            rows=api("/v1/manage/marketplace-ui",{"action":"installed"})["entries"]
            assert sorted(e["icon"] for e in rows if "icon" in e)==sorted(v for v in expected.values() if v),rows
            def has(path,key,name):return any(x.get(key)==name for x in api(path))
            wait(lambda:has("/v1/skills","name","proof-skill"),"skill reload")
            assert has("/v1/agents","id","proof-agent")
            assert has("/v1/skills","name","proof-plugin:plugin-proof")
            result=run("run","--attach",url,"--token",token,"--model","echo/echo-1","!tool ext__marketplace_fixture__ping {}")
            assert "MARKETPLACE_EXTENSION_OK" in result.stdout,result.stdout
            for kind in ("skill","agent","plugin","mcp","extension"):
                api("/v1/manage/marketplace",{"action":"disable","id":ids[kind],"scope":"local"})
            wait(lambda:not has("/v1/skills","name","proof-skill") and not has("/v1/agents","id","proof-agent") and not has("/v1/skills","name","proof-plugin:plugin-proof"),"disabled resources leave runtime")
            # Extensions switched off stay off through an update of one of them.
            extension_settings=workspace/".haarness/extensions.json"
            def extensions_on(value):
                settings=json.loads(extension_settings.read_text());settings["enabled"]=value
                extension_settings.write_text(json.dumps(settings))
            extensions_on(False)
            for kind in ids: marketplace("update",id=ids[kind],scope="local")
            assert json.loads(extension_settings.read_text())["enabled"] is False,"an update turned extensions back on"
            extensions_on(True)
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
            process.terminate();process.wait(timeout=20);process=None

            # Automatic updates. An install of the latest version follows new ones;
            # one that names its version, or says --no-auto-update, stays put.
            follow=publish_skill("proof-follow",1,"FOLLOW_ONE")
            local=publish_skill("proof-local",1,"LOCAL_ONE")
            pinned=publish_skill("proof-pinned",1,"PINNED_ONE")
            optout=publish_skill("proof-optout",1,"OPTOUT_ONE")
            run("marketplace","install","skill",follow,"--global")
            run("marketplace","install","skill",local,"--local")
            run("marketplace","install","skill",pinned,"--local","--version","1")
            run("marketplace","install","skill",optout,"--global","--no-auto-update")
            def receipt(rid): return next(r for r in marketplace("installed") if r["id"]==rid)
            def text(rid): return (Path(receipt(rid)["path"])/"SKILL.md").read_text()
            assert [receipt(r)["autoUpdate"] for r in (follow,local,pinned,optout)]==[True,True,False,False]
            listing=run("marketplace","installed").stdout
            assert "proof-follow\tskill\tversion 1\tall projects\tupdates automatically" in listing,listing
            assert "proof-pinned\tskill\tversion 1\tthis project\tpinned" in listing,listing
            assert run("marketplace","check-updates").stdout.strip()=="Checked 2 resources: all are up to date."
            for rid,slug in ((follow,"proof-follow"),(local,"proof-local"),(pinned,"proof-pinned"),(optout,"proof-optout")):
                publish_skill(slug,2,slug.split("-")[1].upper()+"_TWO",rid)
            out=run("marketplace","check-updates").stdout
            assert "Updated proof-follow to version 2." in out and "Updated proof-local to version 2." in out,out
            assert "FOLLOW_TWO" in text(follow) and "LOCAL_TWO" in text(local)
            assert "PINNED_ONE" in text(pinned) and "OPTOUT_ONE" in text(optout)
            # A rollback by version number stays put until it follows again.
            out=run("marketplace","update",follow,"--version","1").stdout
            assert "Updated proof-follow to version 1." in out and "pinned to version 1" in out,out
            assert receipt(follow)["autoUpdate"] is False and "FOLLOW_ONE" in text(follow)
            assert "Checked 1 resource: it is up to date." in run("marketplace","check-updates").stdout
            assert "FOLLOW_ONE" in text(follow)
            run("marketplace","auto-update",follow)
            assert "Updated proof-follow to version 2." in run("marketplace","check-updates").stdout
            # Turned off and on again from the CLI; the receipt says which.
            assert "stays on version 2" in run("marketplace","auto-update",local,"--local","--off").stdout
            assert receipt(local)["autoUpdate"] is False
            run("marketplace","auto-update",local,"--local")
            assert receipt(local)["autoUpdate"] is True

            # A Git plugin marketplace, registered from a repository that gets a new
            # commit before the server's round.
            upstream,bare=root/"plugins-upstream",root/"plugins-upstream.git"
            def git(*args,cwd=upstream): subprocess.run(["git","-c","init.defaultBranch=main","-c","user.name=Fixture","-c","user.email=fixture@example.invalid",*args],cwd=cwd,check=True,capture_output=True)
            def plugin_commit(text,message):
                (upstream/"kit/commands").mkdir(parents=True,exist_ok=True)
                (upstream/"kit/.claude-plugin").mkdir(parents=True,exist_ok=True)
                (upstream/".claude-plugin").mkdir(exist_ok=True)
                (upstream/".claude-plugin/marketplace.json").write_text(json.dumps({"name":"git-fixture","plugins":[{"name":"kit","source":"./kit"}]}))
                (upstream/"kit/.claude-plugin/plugin.json").write_text('{"name":"kit"}')
                (upstream/"kit/commands/proof.md").write_text(text)
                git("add","-A");git("commit","-qm",message);git("push","-q","origin","HEAD:main")
            upstream.mkdir();git("init","-q","--bare",str(bare),cwd=root);git("init","-q");git("remote","add","origin",str(bare))
            plugin_commit("GIT_PLUGIN_ONE","one")
            added=run("plugin","marketplace","add",f"file://{bare}").stdout
            assert "registered" in added and "It updates automatically; `haarness plugin marketplace auto-update git-fixture --off` stops that." in added,added
            run("plugin","install","kit@git-fixture")
            assert "is up to date" in run("plugin","marketplace","update","git-fixture").stdout
            kit=next(p for p in json.loads(run("marketplace","request",json.dumps({"action":"plugin_marketplaces"})).stdout) if p["name"]=="git-fixture")
            assert kit["type"]=="git" and kit["autoUpdate"] is True,kit
            kit_command=next(data.rglob("cache/git-fixture/kit/commands/proof.md"))
            plugin_commit("GIT_PLUGIN_TWO","two")

            # The server's own round, a minute after it starts.
            publish_skill("proof-follow",3,"FOLLOW_THREE",follow)
            publish_skill("proof-local",3,"LOCAL_THREE",local)
            publish_skill("proof-pinned",3,"PINNED_THREE",pinned)
            server_env={**env,"HAARNESS_AUTO_UPDATE_INTERVAL_SECS":"60"}
            # A round an earlier server of this run did would hold this one off.
            for stamp in [*root.rglob(".auto-update-checked"),*root.rglob("auto-update/*.checked")]: stamp.unlink()
            started=time.time()
            process=subprocess.Popen([*prefix,"serve","--host","127.0.0.1","--port",str(port),"--token",token],env=server_env,stdout=log,stderr=log)
            wait(lambda: api("/v1/info"),"server start for automatic updates")
            assert "FOLLOW_TWO" in text(follow) and kit_command.read_text()=="GIT_PLUGIN_ONE","no round before the first interval"
            for _ in range(240):
                if "FOLLOW_THREE" in text(follow) and "LOCAL_THREE" in text(local) and kit_command.read_text()=="GIT_PLUGIN_TWO": break
                time.sleep(.5)
            else: raise AssertionError("the server's round did not install the new versions")
            took=time.time()-started
            assert 50<took<110,took
            assert receipt(follow)["version"]["number"]==3 and receipt(local)["version"]["number"]==3
            assert "PINNED_ONE" in text(pinned) and "OPTOUT_ONE" in text(optout)
            # The rounds' stamps live in the data folder: nothing new in the project.
            assert not list(workspace.rglob("*auto-update*")),list(workspace.rglob("*auto-update*"))
            assert list(root.rglob("auto-update/*.checked")),"no project stamp in the data folder"
            # The updated skill reaches the runtime too.
            wait(lambda: has("/v1/skills","name","proof-follow"),"updated skill loaded")
            print(f"PASS automatic updates: check-updates installs new versions of installs that follow them, the server's round did so (and fetched a Git plugin marketplace's new commit) {took:.0f}s after start, pinned and opted-out installs stayed")
        finally:
            if process: process.terminate();process.wait(timeout=20)
            catalog.shutdown()
            run("serve","--stop",ok=False)

if __name__=="__main__": main()
