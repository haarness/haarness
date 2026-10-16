#!/usr/bin/env python3
"""Logout proof through real workspace daemons, HTTP MCP and CLI logout."""
import concurrent.futures
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

import e2e_scratch

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
ENTERED = threading.Event()
RELEASE = threading.Event()
CALLS = []


class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, code, value):
        raw = json.dumps(value).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except BrokenPipeError:
            pass

    def do_GET(self):
        base = f"http://127.0.0.1:{self.server.server_port}"
        if "oauth-protected-resource" in self.path:
            self.reply(200, {"resource": base + ("/pending" if self.path.endswith("/pending") else "/mcp"), "authorization_servers": [base]})
        elif ".well-known" in self.path:
            self.reply(200, {"issuer": base, "authorization_endpoint": base + "/authorize", "token_endpoint": base + "/token", "code_challenge_methods_supported": ["S256"]})
        else:
            self.reply(405, {})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.headers.get("Authorization") != "Bearer fixture-token":
            self.send_response(401)
            self.send_header("WWW-Authenticate", f'Bearer resource_metadata="http://127.0.0.1:{self.server.server_port}/.well-known/oauth-protected-resource"')
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if "id" not in body:
            self.reply(202, {})
            return
        method = body["method"]
        if method == "initialize":
            result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "logout-fixture", "version": "1"}}
        elif method == "tools/list":
            if self.path == "/pending":
                ENTERED.set()
                RELEASE.wait(30)
            result = {"tools": [{"name": "echo", "inputSchema": {"type": "object"}, "description": "fixture"}]}
        elif method == "tools/call":
            CALLS.append(self.path)
            result = {"content": [{"type": "text", "text": "fixture call succeeded"}]}
        else:
            result = {}
        self.reply(200, {"jsonrpc": "2.0", "id": body["id"], "result": result})


def run(args, env, **kwargs):
    return subprocess.run([str(BINARY), *args], env=env, check=True, capture_output=True, text=True, timeout=60, **kwargs).stdout


def main():
    service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    threading.Thread(target=service.serve_forever, daemon=True).start()
    root = Path(tempfile.mkdtemp(prefix="haarness-logout-workspaces-")).resolve()
    env = {**os.environ, "HAARNESS_HOME": str(root / "home"), "HAARNESS_DATA_DIR": str(root / "data"), "HAARNESS_TEST_PROVIDER": "1"}
    url = f"http://127.0.0.1:{service.server_port}"
    credentials = root / "data/data/mcp-oauth" / ("mcp-" + "secure".encode().hex() + ".json")
    credentials.parent.mkdir(parents=True)

    def signin():
        credentials.write_text(json.dumps({"client_id": "fixture-client", "token_response": {"access_token": "fixture-token", "token_type": "Bearer", "expires_in": 3600}, "granted_scopes": [], "token_received_at": int(time.time()), "issuer": url}))
        credentials.chmod(0o600)

    signin()
    clients = []
    workspaces = []
    try:
        for index in range(4):
            workspace = root / f"workspace-{index}"
            workspaces.append(workspace)
            (workspace / ".haarness").mkdir(parents=True)
            config = '[defaults]\nprovider="echo"\nmodel="echo-1"\n'
            if index == 0:
                config += f'\n[mcp.secure]\ntransport="streamable_http"\nurl="{url}/mcp"\noauth=true\nenabled=true\n'
            (workspace / ".haarness/config.toml").write_text(config)
            connection = json.loads(run(["--workspace", str(workspace), "--trust-project", "desktop"], env, input='{"action":"connect"}'))

            def api(path, body=None, connection=connection):
                request = urllib.request.Request(connection["url"] + "/v1" + path, data=None if body is None else json.dumps(body).encode(), headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
                with urllib.request.urlopen(request, timeout=40) as response:
                    raw = response.read()
                    return json.loads(raw) if raw else None
            clients.append(api)
        config = {"transport": "streamable_http", "url": url + "/mcp", "enabled": True, "oauth": True, "timeout_ms": 30000}
        assert clients[1]("/mcp/secure", config)["status"] == "connected"
        assert clients[3]("/mcp/secure", {**config, "headers": {"Authorization": "Bearer fixture-token"}})["status"] == "connected"
        for index in (0, 1, 3):
            assert clients[index]("/mcp/secure/tools/echo", {"arguments": {}})
        with concurrent.futures.ThreadPoolExecutor() as pool:
            pending = pool.submit(clients[2], "/mcp/secure", {**config, "url": url + "/pending"})
            assert ENTERED.wait(10), f"pending discovery never reached MCP: {pending.result() if pending.done() else 'still waiting'}"
            output = run(["--workspace", str(workspaces[0]), "--trust-project", "mcp", "logout", "secure", "--project"], env)
            assert "Cleared OAuth credentials" in output
            assert not credentials.exists()
            RELEASE.set()
            try:
                result = pending.result(15)
                assert result["status"] != "connected", result
            except urllib.error.HTTPError:
                pass
        before = len(CALLS)
        for index in (0, 1, 2):
            assert clients[index]("/mcp/secure/tools") == []
            try:
                clients[index]("/mcp/secure/tools/echo", {"arguments": {}})
                raise AssertionError("logged-out client executed a tool")
            except urllib.error.HTTPError:
                pass
        assert len(CALLS) == before, "a blocked call reached the MCP server"
        assert clients[3]("/mcp/secure/tools/echo", {"arguments": {}}), "explicit authorization was revoked"
        signin()
        assert clients[1]("/mcp/secure/reconnect", {})["status"] == "connected"
        assert clients[1]("/mcp/secure/tools/echo", {"arguments": {}})
        print(json.dumps({"passed": ["four workspace daemons", "CLI logout", "live catalogs invalidated", "late discovery rejected", "calls blocked without replay", "explicit header preserved", "reauthentication"], "fixture": str(root)}, indent=2))
    finally:
        RELEASE.set()
        for workspace in workspaces:
            try:
                run(["--workspace", str(workspace), "serve", "--stop"], env)
            except Exception:
                pass
        service.shutdown()
        e2e_scratch.settle(root)


if __name__ == "__main__":
    main()
