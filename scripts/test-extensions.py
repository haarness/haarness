#!/usr/bin/env python3
"""Real CLI/server/Bun/provider-wire/TUI extension lifecycle, in private fixtures."""
import http.server
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tarfile
import threading
import time
import urllib.request

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()
REQUESTS = []
PACKAGES = {}


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        name = self.path.strip("/").split("/")[0]
        if name in PACKAGES:
            manifest, archive = PACKAGES[name]
            if self.path.endswith(".tgz"):
                content = archive
                mime = "application/octet-stream"
            else:
                version = {**manifest, "dist": {"tarball": f"http://127.0.0.1:{self.server.server_port}/{name}/-/{name}-1.0.0.tgz", "shasum": hashlib.sha1(archive).hexdigest()}}
                content = json.dumps({"name": name, "dist-tags": {"latest": "1.0.0"}, "versions": {"1.0.0": version}}).encode()
                mime = "application/json"
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"data": [{"id": "fixture-model", "object": "model"}]}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        REQUESTS.append(body)
        messages = body["messages"]
        user = next(m for m in reversed(messages) if m["role"] == "user")
        text = user["content"]
        if isinstance(text, list):
            text = " ".join(p.get("text", "") for p in text)
        tail = messages[messages.index(user) + 1:]
        result = next((m["content"] for m in reversed(tail) if m["role"] == "tool"), None)
        if result is not None:
            delta = {"content": "WIRE_RESULT " + str(result)}
            finish = "stop"
        elif "USE_EXTENSION" in text:
            delta = {"tool_calls": [{"index": 0, "id": "fixture-call", "type": "function", "function": {
                "name": "ext__fixture__greet", "arguments": json.dumps({"name": "before"})}}]}
            finish = "tool_calls"
        else:
            system = " ".join(str(m["content"]) for m in messages if m["role"] in ("system", "developer"))
            assert "SYSTEM_EXTENSION_V1" in system, system
            # Prompt-hook context travels with the turn, after the prompt, so
            # the system prompt stays byte-identical from turn to turn.
            assert "PROMPT_EXTENSION" not in system, system
            users = " ".join(str(m["content"]) for m in messages if m["role"] == "user")
            assert "PROMPT_EXTENSION" in users, messages
            # chat.params and chat.headers reach the wire for OpenAI-compatible providers.
            assert body.get("temperature") == 0.1, body
            assert body.get("fixture_option") == "yes", body
            assert self.headers.get("x-fixture-extension") == "yes", dict(self.headers)
            delta = {"content": "CHAT_HOOKS_REACHED_PROVIDER"}
            finish = "stop"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for chunk in [{"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                      {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}], "usage": {"prompt_tokens": 120, "completion_tokens": 30}}]:
            self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")


def wait_until(predicate, label, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.15)
    raise AssertionError("Timed out: " + label)


def package(name, manifest, source):
    manifest = {"name": name, "version": "1.0.0", "type": "module", **manifest}
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w:gz") as archive:
        for filename, content in {"package.json": json.dumps(manifest), "index.ts": source}.items():
            data = content.encode()
            info = tarfile.TarInfo("package/" + filename)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    PACKAGES[name] = (manifest, stream.getvalue())


def main():
    assert BINARY.is_file(), "build haarness-cli first"
    assert shutil.which("bun") or os.environ.get("HAARNESS_BUN"), "Bun required"
    with tempfile.TemporaryDirectory(prefix="haarness-extensions-e2e-") as tmp:
        root = Path(tmp).resolve()
        workspace = root / "workspace"
        data = root / "state"
        home = root / "home"
        for path in (workspace, data, home):
            path.mkdir()
        env = {key: value for key, value in os.environ.items() if key in ("PATH", "TMPDIR", "TMP", "TEMP", "SYSTEMROOT", "WINDIR", "LANG", "LC_ALL", "TERM", "COLORTERM")}
        env["TERM"] = "xterm-256color"
        env.update(HOME=str(home), HAARNESS_TEST_PROVIDER="1", RUST_LOG="warn")
        if os.environ.get("HAARNESS_BUN"):
            env["HAARNESS_BUN"] = os.environ["HAARNESS_BUN"]
        subprocess.run(["git", "init", "-q", str(workspace)], check=True, env=env)
        (workspace / ".gitignore").write_text("events.jsonl\nsessions.jsonl\ninflight\ndelay\nPROJECT_LOADED\nhook-wait\nwait-post\npost-wait\n")
        prefix = [str(BINARY), "--data-dir", str(data), "--workspace", str(workspace)]

        def run(*args, ok=True):
            result = subprocess.run([*prefix, *args], env=env, text=True, capture_output=True, timeout=60)
            if ok:
                assert result.returncode == 0, (args, result.stdout, result.stderr)
            return result.stdout + result.stderr

        provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        threading.Thread(target=provider.serve_forever, daemon=True).start()
        process = None
        session = "haarness-extension-" + str(os.getpid())
        try:
            starter = root / "starter"
            run("extension", "init", str(starter))
            assert (starter / "api.d.ts").is_file()
            run("extension", "install", str(starter))
            check = run("extension", "check")
            # The starter is named after its folder.
            assert "ext__starter__greet" in check
            run("extension", "remove", "1")
            run("extension", "disable")

            # Real registry install, transitive dependency, lockfile, lifecycle-script suppression.
            package("fixture-helper", {"main": "index.ts"}, "export const greeting = 'NPM_DEPENDENCY_OK';")
            package("fixture-extension", {"haarness": {"extension": "index.ts"}, "dependencies": {"fixture-helper": "1.0.0"},
                    "scripts": {"postinstall": "touch " + str(root / "UNEXPECTED_INSTALL_SCRIPT")}},
                    "import {greeting} from 'fixture-helper';export default {apiVersion:1,name:'npm_fixture',tools:{ping:{description:greeting,parameters:{type:'object'},execute(){return greeting}}}};")
            (home / ".npmrc").write_text(f"registry=http://127.0.0.1:{provider.server_port}\n")
            run("extension", "install", "npm:fixture-extension@1.0.0")
            assert "NPM_DEPENDENCY_OK" in run("extension", "check")
            assert not (root / "UNEXPECTED_INSTALL_SCRIPT").exists()
            assert list((data / "data/extensions/packages").glob("*/bun.lock"))
            result = run("run", "--model", "echo/echo-1", "!tool ext__npm_fixture__ping {}")
            assert "NPM_DEPENDENCY_OK" in result, result
            run("serve", "--stop")
            run("extension", "remove", "1")

            # Trust gating: an untrusted project's initializer must never run.
            project = workspace / ".haarness/extensions"
            project.mkdir(parents=True)
            (project / "project.ts").write_text("import {writeFileSync} from 'node:fs';writeFileSync('PROJECT_LOADED','yes');export default {apiVersion:1,name:'project'};")
            run("--disable-project-config", "extension", "check")
            assert not (workspace / "PROJECT_LOADED").exists()
            run("extension", "enable")
            run("--disable-project-config", "extension", "check")
            assert not (workspace / "PROJECT_LOADED").exists()
            assert "project" in run("--trust-project", "extension", "check")
            assert (workspace / "PROJECT_LOADED").is_file()
            shutil.rmtree(project)
            bundle = root / "bundle"
            (bundle / ".claude-plugin").mkdir(parents=True)
            (bundle / "extensions").mkdir()
            (bundle / ".claude-plugin/plugin.json").write_text('{"name":"bundle_fixture","version":"1.0.0"}')
            (bundle / "extensions/addon.ts").write_text("export default {apiVersion:1,name:'bundle_fixture'};")
            run("plugin", "install", str(bundle))
            assert "bundle_fixture" in run("extension", "check")
            run("plugin", "disable", "bundle_fixture")
            assert "bundle_fixture" not in run("extension", "check")

            plugin = root / "fixture"
            plugin.mkdir()
            source = plugin / "index.ts"
            source.write_text('''import {appendFileSync, writeFileSync, existsSync} from 'node:fs';
export default (ctx) => ({
  apiVersion: 1, name: 'fixture',
  tools: { greet: { description: 'Fixture greeting',
    parameters:{type:'object',properties:{name:{type:'string'}},required:['name'],additionalProperties:false},
    async execute(args: {name:string}, context) {
      if(existsSync(context.workspace+'/delay')) {writeFileSync(context.workspace+'/inflight','yes');await Bun.sleep(1600);}
      writeFileSync(context.workspace+'/greeting.txt',ctx.options.prefix+' '+args.name+' V1');
      return ctx.options.prefix+' '+args.name+' V1';
    }
  }},
  hooks: {
    async prompt(input) { if(input.prompt==='WAIT_PROMPT'){writeFileSync(ctx.workspace+'/hook-wait','yes');await new Promise(()=>{});} if(input.prompt==='BLOCK_PROMPT')return {block:'PROMPT_BLOCKED'}; if(input.prompt==='CHAT_ONLY')return {context:'PROMPT_EXTENSION'}; },
    chat() { return {context:'SYSTEM_EXTENSION_V1'}; },
    'chat.params'() { return {temperature:0.1, options:{fixture_option:'yes'}}; },
    'chat.headers'() { return {headers:{'x-fixture-extension':'yes'}}; },
    'permission.ask'(input) { if(input.permission==='ext__fixture__greet') return {status:'deny', reason:'PERMISSION_ASK_DENIED'}; },
    'shell.env'() { return {env:{FIXTURE_SHELL_ENV:'from-extension'}}; },
    'session.start'(input) { appendFileSync(ctx.workspace+'/sessions.jsonl',JSON.stringify(input)+'\\n'); },
    'tool.before'(input) { if(input.tool==='ext__fixture__greet')return {arguments:{name:'rewritten'}}; },
    async 'tool.after'(input) { if(input.tool==='ext__fixture__greet'){if(existsSync(ctx.workspace+'/wait-post')){writeFileSync(ctx.workspace+'/post-wait','yes');await new Promise(()=>{});}return {context:'AFTER_EXTENSION'};} },
    event(input) { appendFileSync(ctx.workspace+'/events.jsonl',JSON.stringify(input)+'\\n'); }
  }
});
''')
            run("extension", "install", str(plugin), "--options", '{"prefix":"E2E"}')
            run("provider", "add", "fixture", "--url", f"http://127.0.0.1:{provider.server_port}/v1", "--no-auth", "--model", "fixture-model")
            # Pin extension tools resident for the deterministic provider fixture.
            with (data / "config/config.toml").open("a") as config:
                config.write('\n[lazy_tools]\npin = ["ext__*"]\n')
            settings = data / "config/haarness.json"
            existing = json.loads(settings.read_text()) if settings.exists() else {}
            existing.update(model="echo/echo-1", permission={"ext__fixture__greet":"allow", "bash":"allow"})
            settings.write_text(json.dumps(existing))
            token = "fixture-extension-token"
            logs = open(root / "server.log", "w")
            process = subprocess.Popen([*prefix, "serve", "--port", "0", "--token", token], env=env, stdout=logs, stderr=logs)

            def discovery():
                files = list((data / "cache/workspaces").glob("*/runtime.json"))
                return files[0] if files else None

            wait_until(discovery, "server discovery")
            runtime = json.loads(discovery().read_text())
            url = runtime["url"]

            def api(path, value=None):
                request = urllib.request.Request(url + path, headers={"Authorization": "Bearer " + token},
                                                 data=None if value is None else json.dumps(value).encode())
                if value is not None:
                    request.add_header("Content-Type", "application/json")
                with urllib.request.urlopen(request, timeout=10) as response:
                    body = response.read()
                    return json.loads(body) if body else None

            def catalog_has_tool():
                return "ext__fixture__greet" in json.dumps(api("/v1/tools"))

            wait_until(catalog_has_tool, "extension catalog")
            result = run("run", "--attach", url, "--token", token, "--model", "fixture/fixture-model", "--title", "Prepared request hooks", "CHAT_ONLY")
            assert "CHAT_HOOKS_REACHED_PROVIDER" in result, result
            accounting = next(item for item in api("/v1/threads") if item["title"] == "Prepared request hooks")
            reports = [item["data"] for item in api("/v1/threads/" + accounting["id"] + "/items") if item["data"]["type"] == "usage"]
            assert reports and reports[-1]["input_tokens"] == 120
            prepared = reports[-1]["prompt"]
            request = next(request for request in reversed(REQUESTS) if any(message.get("content") == "CHAT_ONLY" for message in request["messages"]))
            system = " ".join(message["content"] for message in request["messages"] if message["role"] in ("system", "developer"))
            # The estimator weighs character classes per tokenizer family, so
            # it lands near, not exactly on, four characters per token.
            assert len(system) / 8 < prepared["system_tokens"] < len(system) / 2, (prepared, len(system))
            assert prepared["tool_tokens"] > 0 and prepared["history_tokens"] > 0
            assert prepared["media_attachments"] == 0

            result = run("run", "--attach", url, "--token", token, "--model", "fixture/fixture-model", "--title", "Extension wire", "USE_EXTENSION")
            assert "E2E rewritten V1" in result and "AFTER_EXTENSION" in result, result
            assert (workspace / "greeting.txt").read_text() == "E2E rewritten V1"
            thread = next(item for item in api("/v1/threads") if item["title"] == "Extension wire")
            api("/v1/threads/" + thread["id"] + "/revert", {})
            assert not (workspace / "greeting.txt").exists(), "undo restores pre-extension workspace"
            api("/v1/threads/" + thread["id"] + "/unrevert", {})
            assert (workspace / "greeting.txt").read_text() == "E2E rewritten V1"
            assert any("ext__fixture__greet" in json.dumps(request.get("tools", [])) for request in REQUESTS)
            result = run("run", "--attach", url, "--token", token, "--model", "echo/echo-1", "BLOCK_PROMPT", ok=False)
            assert "PROMPT_BLOCKED" in result, result

            # Invalid code retains the running generation; corrected code hot reloads.
            original = source.read_text()
            source.write_text("export default BROKEN SYNTAX;")
            wait_until(lambda: "hot reload rejected" in (root / "server.log").read_text(), "invalid replacement reported")
            assert catalog_has_tool()
            result = run("run", "--attach", url, "--token", token, "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"again"}')
            assert "E2E rewritten V1" in result, result
            source.write_text(original)
            time.sleep(1)
            (workspace / "delay").write_text("yes")
            active = subprocess.Popen([*prefix, "run", "--attach", url, "--token", token, "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"active"}'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            wait_until(lambda: (workspace / "inflight").exists(), "active extension call")
            source.write_text(original.replace(" V1", " V2"))
            output, error = active.communicate(timeout=20)
            assert active.returncode == 0 and "E2E rewritten V1" in output, (output, error)
            (workspace / "delay").unlink()

            def updated():
                result = run("run", "--attach", url, "--token", token, "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"again"}')
                return "E2E rewritten V2" in result

            wait_until(updated, "corrected extension generation", timeout=30)
            events = [json.loads(line)["type"] for line in (workspace / "events.jsonl").read_text().splitlines()]
            assert {"prompt", "chat", "chat.params", "chat.headers", "tool.before", "tool.after", "stop", "session.start", "chat.turn.end", "config"} <= set(events), events
            sessions = [json.loads(line) for line in (workspace / "sessions.jsonl").read_text().splitlines()]
            assert sessions and sessions[0]["directory"] == str(workspace) and sessions[0]["agent"], sessions
            # shell.env reaches the bash tool; permission.ask can deny before the prompt exists.
            result = run("run", "--attach", url, "--token", token, "--model", "echo/echo-1", '!tool bash {"command":"printf %s $FIXTURE_SHELL_ENV"}')
            assert "from-extension" in result, result
            events = [json.loads(line)["type"] for line in (workspace / "events.jsonl").read_text().splitlines()]
            assert "shell.env" in events, events
            (data / "config/haarness.json").write_text('{"model":"echo/echo-1","permission":{"ext__fixture__greet":"ask"}}')
            def asked_and_denied():
                result = run("run", "--attach", url, "--token", token, "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"ASK_DENY"}', ok=False)
                return "PERMISSION_ASK_DENIED" in result
            wait_until(asked_and_denied, "permission.ask denial")
            (data / "config/haarness.json").write_text('{"model":"echo/echo-1","permission":{"ext__fixture__greet":"allow"}}')
            wait_until(updated, "permission restored after permission.ask")

            # Permission deny must beat an extension's tool.before rewrite.
            (data / "config/haarness.json").write_text('{"model":"echo/echo-1","permission":{"ext__fixture__greet":"deny"}}')
            def denied():
                before = (workspace / "greeting.txt").stat().st_mtime_ns
                result = run("run", "--attach", url, "--token", token, "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"denied"}', ok=False)
                rejected = any(word in result.lower() for word in ("denied", "disabled", "not available", "not advertised"))
                if rejected:
                    assert (workspace / "greeting.txt").stat().st_mtime_ns == before
                return rejected
            wait_until(denied, "permission rule applied")
            (data / "config/haarness.json").write_text('{"model":"echo/echo-1","permission":{"ext__fixture__greet":"allow"}}')
            wait_until(updated, "permission restored")
            before = (workspace / "greeting.txt").stat().st_mtime_ns
            result = run("run", "--attach", url, "--token", token, "--agent", "plan", "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"plan"}', ok=False)
            assert any(word in result.lower() for word in ("denied", "disabled", "not available", "not advertised")), result
            assert (workspace / "greeting.txt").stat().st_mtime_ns == before

            # Real rendered TUI: extension tool output is visible through remote attach.
            assert shutil.which("tmux"), "tmux required for rendered TUI proof"
            import shlex
            command = shlex.join(["env", "-i", *[f"{key}={value}" for key, value in env.items()], *prefix, "attach", url, "--token", token])
            subprocess.run(["tmux", "new-session", "-d", "-s", session, "-x", "140", "-y", "42", command], check=True)
            def screen():
                return subprocess.check_output(["tmux", "capture-pane", "-p", "-t", session], text=True)
            wait_until(lambda: "Connected" in screen() and "Ask anything" in screen(), "TUI launch")
            # Leading space keeps the echo fixture directive out of TUI shell mode.
            subprocess.run(["tmux", "send-keys", "-t", session, "-l", ' !tool ext__fixture__greet {"name":"terminal"}'], check=True)
            subprocess.run(["tmux", "send-keys", "-t", session, "Enter"], check=True)
            wait_until(lambda: "E2E rewritten V2" in screen(), "TUI extension result")
            subprocess.run(["tmux", "kill-session", "-t", session], check=True)

            hanging = subprocess.Popen([*prefix, "run", "--attach", url, "--token", token, "--title", "Cancel extension hook", "--model", "echo/echo-1", "WAIT_PROMPT"], env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_until(lambda: (workspace / "hook-wait").exists(), "pending prompt hook")
            thread = next(item for item in api("/v1/threads") if item["title"] == "Cancel extension hook")
            api("/v1/threads/" + thread["id"] + "/interrupt", {})
            output, error = hanging.communicate(timeout=5)
            assert hanging.returncode != 0, (output, error)
            assert api("/v1/info")["name"] == "haarness"

            run("extension", "disable")
            wait_until(lambda: not catalog_has_tool(), "disabled extension removal")
            run("extension", "enable")
            wait_until(catalog_has_tool, "re-enabled extension")
            (workspace / "wait-post").write_text("yes")
            hanging = subprocess.Popen([*prefix, "run", "--attach", url, "--token", token, "--title", "Cancel post-tool hook", "--model", "echo/echo-1", '!tool ext__fixture__greet {"name":"post"}'], env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_until(lambda: (workspace / "post-wait").exists(), "pending post-tool hook")
            thread = next(item for item in api("/v1/threads") if item["title"] == "Cancel post-tool hook")
            api("/v1/threads/" + thread["id"] + "/interrupt", {})
            output, error = hanging.communicate(timeout=5)
            assert hanging.returncode != 0, (output, error)
            assert api("/v1/info")["name"] == "haarness"
            run("extension", "remove", "fixture")
            wait_until(lambda: not catalog_has_tool(), "removed extension")
            assert source.is_file(), "removal must preserve user source"
            api("/v1/shutdown", {})
            process.wait(timeout=10)
            assert process.returncode == 0
            print("PASS: scaffold, trust, TS tools, wire hooks, events, permissions, reload, TUI, disable/remove, shutdown")
        except Exception:
            if (root / "server.log").exists():
                print((root / "server.log").read_text()[-12000:])
            raise
        finally:
            if shutil.which("tmux"):
                subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            provider.shutdown()


if __name__ == "__main__":
    main()
