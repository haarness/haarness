#!/usr/bin/env python3
"""Identical behavioral scenarios through TS, Python sync and Python async SDKs.

No raw HTTP substitutes for SDK operations. The controller only prepares local
fixtures, checks filesystem outcomes, and records success coverage per method.
"""

import base64
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import threading
import time
import jsonschema
from uuid import uuid4
from server import Fixture, ROOT, BINARY


class Runner:
    def __init__(self, mode, fixture):
        python = Path(
            os.environ.get("SDK_PYTHON", ROOT / "sdks/python/.venv/bin/python")
        )
        args = (
            ["node", str(ROOT / "sdks/tests/runner.mjs")]
            if mode == "typescript"
            else [str(python), str(ROOT / "sdks/tests/runner.py"), mode]
        )
        self.process = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env={**os.environ, "SDK_URL": fixture.url, "SDK_TOKEN": fixture.token},
        )
        self.responses = queue.Queue()
        self.errors = []
        self.covered = set()
        self.spec = json.loads((ROOT / "sdks/openapi.json").read_text())
        self.operations = {
            op["name"]: op
            for op in json.loads((ROOT / "sdks/operations.json").read_text())
        }

        def stdout():
            for line in self.process.stdout:
                self.responses.put(line)
            self.responses.put(None)

        def stderr():
            for line in self.process.stderr:
                self.errors.append(line)

        threading.Thread(target=stdout, daemon=True).start()
        threading.Thread(target=stderr, daemon=True).start()

    def call(self, name, *args, error=None, **kwargs):
        self.process.stdin.write(
            json.dumps({"name": name, "args": args, "kwargs": kwargs}) + "\n"
        )
        self.process.stdin.flush()
        line = self.responses.get(timeout=60)
        assert line is not None, "".join(self.errors)[-3000:]
        result = json.loads(line)
        if error is not None:
            assert not result["ok"] and result.get("status") == error, (name, result)
            return result
        assert result["ok"], (name, result)
        op = self.operations.get(name)
        if op and not op.get("stream") and not op.get("binaryResponse"):
            jsonschema.validate(
                result.get("value"),
                {**op["response"], "components": self.spec["components"]},
            )
        self.covered.add(name)
        return result.get("value")

    def until(self, name, *args, predicate=lambda x: bool(x)):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            value = self.call(name, *args)
            if predicate(value):
                return value
            time.sleep(0.05)
        raise AssertionError("Timed out: " + name)

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise AssertionError("SDK runner did not release its resources")
        assert self.process.returncode == 0, "SDK runner failed during cleanup"
        self.process.stdout.close()
        self.process.stderr.close()


def scenario(mode):
    with Fixture() as fixture:
        runner = Runner(mode, fixture)
        c = runner.call
        try:
            assert c("health")["status"] == "ok"
            assert "components" in c("openapi")
            assert c("info")["workspace"] == str(fixture.workspace)
            for name in (
                "providers",
                "refresh_providers",
                "agents",
                "commands",
                "lsp_status",
                "formatter_status",
                "tool_catalog",
                "projects",
                "workspaces_list",
                "workspace_refs",
                "workflows",
                "skills",
                "plugins",
                "mcp_servers",
                "list_threads",
                "list_ptys",
                "active_turns",
                "list_permissions",
                "list_questions",
            ):
                c(name)
            c("host_projects")
            assert c("host_directories", {"path":str(fixture.workspace)})["path"] == str(fixture.workspace)
            opened = c("host_open", {"path":str(fixture.workspace)})
            assert opened["path"] == str(fixture.workspace)
            c("host_close", opened["id"])
            assert all(row["id"] != opened["id"] for row in c("host_projects")["workspaces"])
            assert c("host_open", {"path":str(fixture.workspace)})["id"] == opened["id"]
            assert c("list_files", {"path": ""})["entries"]
            file_cursor = c("info")["eventCursor"]
            document = c("read_file", {"path": "fixture.txt"})
            updated = c("write_file", {"path": "fixture.txt", "content": "edited via SDK\n", "revision": document["revision"]})
            assert updated["content"] == "edited via SDK\n"
            c("write_file", {"path": "fixture.txt", "content": "stale", "revision": document["revision"]}, error=409)
            c("write_file", {"path": "fixture.txt", "content": "before\n", "revision": updated["revision"]})
            c("read_file", {"path": "../outside"}, error=400)
            c("mutate_file", {"action": "mkdir", "path": "empty-dir"})
            c("mutate_file", {"action": "rename", "path": "empty-dir", "target": "renamed-dir"})
            c("mutate_file", {"action": "delete", "path": "renamed-dir", "confirm": True})
            file_events = c("events", file_cursor, 5)
            assert all(e["type"] == "files.updated" for e in file_events), file_events
            assert [e["payload"]["path"] for e in file_events] == ["fixture.txt", "fixture.txt", "empty-dir", "empty-dir", "renamed-dir"]

            assert c("bad_auth", error=401)["retryable"] is False
            c("get_thread", str(uuid4()), error=404)
            c("create_thread", {"title": []}, error=422)
            thread = c(
                "create_thread",
                {"title": "SDK lifecycle", "provider": "echo", "model": "echo-1"},
            )
            tid = thread["id"]
            assert c("get_thread", tid)["id"] == tid
            with fixture.browser_host(tid) as browser_calls:
                browser = c("call_browser_tool", tid, {
                    "name": "browser_tabs_list", "arguments": {},
                })
                assert browser["isError"] is False, browser
                assert json.loads(browser["output"])["tabs"] == []
                assert len(browser_calls) == 2
                assert browser_calls[0]["inspect"] is True
                c("call_browser_tool", tid, {"name": "bash", "arguments": {}}, error=400)
            host = c("connect_browser_host", tid)
            assert host["type"] == "attached"
            # Real TCP tunnel to the fixture server through the browser host API.
            from urllib.parse import urlsplit
            target = {"host": "127.0.0.1", "port": urlsplit(fixture.url).port}
            tunnel = c("browser_tunnel", tid, "open", {"lease":host["lease"], "target": target})
            c("browser_tunnel", tid, "write", {"lease":host["lease"], "id": tunnel, "data": base64.b64encode(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").decode()})
            data = c("browser_tunnel", tid, "read", {"lease":host["lease"], "id": tunnel})
            assert b"200 OK" in base64.b64decode(data["data"])
            c("browser_tunnel", tid, "close", {"lease":host["lease"], "id": tunnel})
            draft = str(uuid4())
            host = c("connect_draft_browser_host", draft)
            lease = host["lease"]
            tunnel = c("draft_browser_tunnel", draft, "open", {"lease": lease, "target": target})
            c("draft_browser_tunnel", draft, "close", {"lease": lease, "id": tunnel})
            adopted_thread = c("create_thread", {"title":"Draft browser adoption"})["id"]
            assert c("adopt_browser_draft", draft, {"thread": adopted_thread, "lease": lease})["adopted"]
            c("update_thread", adopted_thread, {"archived": True})
            c("draft_browser_tunnel", draft, "close", {"lease": "incorrect-lease", "id": tunnel}, error=400)
            # A pre-existing published share is read through the SDK's HTML route.
            share = uuid4().hex + uuid4().hex
            shares = fixture.data / "data" / "shares"
            shares.mkdir(parents=True, exist_ok=True)
            (shares / (share + ".json")).write_text(json.dumps({"token":share,"threadId":tid,"title":"SDK shared chat","createdAt":"2026-09-28T00:00:00Z","expiresAt":"2099-01-01T00:00:00Z","messages":[{"role":"You","text":"SDK_SHARE_PROOF"}]}))
            assert "SDK_SHARE_PROOF" in c("read_shared_session", share)
            assert c("update_thread", tid, {"title": "Renamed"})["title"] == "Renamed"
            cursor = c("info")["eventCursor"]
            turn = c(
                "start_turn", tid, {"requestId": str(uuid4()), "text": "Hello SDK"}
            )
            assert c("get_turn", turn["id"])["id"] == turn["id"]
            assert (
                c("wait_turn", turn["id"], {"timeout_ms": 10000})["status"]
                == "completed"
            )
            assert c("active_turn", tid) is None
            items = c("list_items", tid)
            assert any(x["data"].get("text") == "Echo: Hello SDK" for x in items)
            assert c("list_items", tid, {"after": items[-1]["sequence"]}) == []
            assert c("thread_usage", tid)["outputTokens"] > 0
            assert c("manage_resource", "profiles", {"action": "view"})["title"] == "Configuration profiles"
            assert "skills" in c("capabilities")
            c("capabilities", {"action":"create","kind":"skill","name":"sdk-marketplace","scope":"local","description":"SDK marketplace proof","instructions":"Return SDK marketplace proof."})
            runner.until("skills", predicate=lambda rows:any(row["name"]=="sdk-marketplace" for row in rows))
            c("capabilities", {"action":"disable","kind":"skill","name":"sdk-marketplace","scope":"local"})
            runner.until("skills", predicate=lambda rows:all(row["name"]!="sdk-marketplace" for row in rows))
            c("capabilities", {"action":"enable","kind":"skill","name":"sdk-marketplace","scope":"local"})
            runner.until("skills", predicate=lambda rows:any(row["name"]=="sdk-marketplace" for row in rows))
            c("capabilities", {"action":"remove","kind":"skill","name":"sdk-marketplace","scope":"local"})
            resource_id,version_id=str(uuid4()),str(uuid4())
            manifest={"manifestVersion":1,"resource":{"id":resource_id,"slug":"sdk-proof","kind":"mcp","name":"SDK proof"},"version":{"id":version_id,"number":1,"status":"approved","yanked":False},"lock":{"resourceId":resource_id,"versionId":version_id},"install":{"mcp":{"config":{"type":"remote","url":"http://127.0.0.1:9/mcp","enabled":False},"env_placeholders":[]}},"compatibility":{},"capabilities":{}}
            c("marketplace",{"action":"install","manifest":manifest,"scope":"local"})
            assert any(row["id"]==resource_id for row in c("marketplace",{"action":"installed"}))
            for action in ("disable","enable","remove"):
                c("marketplace",{"action":action,"id":resource_id,"scope":"local"})
            assert not c("marketplace",{"action":"installed"})
            events = c("events", cursor, 2)
            assert (
                len(events) == 2
                and events[0]["eventId"] > cursor
                and events[1]["eventId"] > events[0]["eventId"]
            )
            replay = c("events", events[0]["eventId"], 1)
            assert replay[0]["eventId"] == events[1]["eventId"]
            chat = c("chat", tid, "Convenience chat")
            assert (
                chat["text"] == "Echo: Convenience chat"
                and chat["turn"]["status"] == "completed"
            ), chat
            # Idempotency: the exact request id does not start another turn.
            request = {"requestId": str(uuid4()), "text": "Idempotent"}
            first = c("start_turn", tid, request)
            second = c("start_turn", tid, request)
            assert first["id"] == second["id"]
            c("wait_turn", first["id"], {"timeout_ms": 10000})
            # Real prompts and lifecycle controls.
            slow = c("start_turn", tid, {"requestId": str(uuid4()), "text": "::mdslow"})
            assert c("active_turns")
            c("wait_turn", slow["id"], {"timeout_ms": 1}, error=409)
            queued = c(
                "queue_input", tid, {"text": "Queued input", "requestId": str(uuid4())}
            )
            assert queued["queued"]
            c("interrupt_turn", slow["id"])
            c("wait_turn", slow["id"], {"timeout_ms": 10000})
            c("interrupt_thread_tree", tid)
            # Interrupt acknowledges cancellation; a queued follow-up may still
            # be unwinding. Wait for durable idle before starting a new turn.
            deadline = time.monotonic() + 10
            while c("active_turn", tid) is not None:
                assert time.monotonic() < deadline, "thread did not settle after interruption"
                time.sleep(0.05)
            # Pending question + reply, then permission + reply using the actual tools.
            qturn = c(
                "start_turn",
                tid,
                {
                    "requestId": str(uuid4()),
                    "text": '!tool question {"prompt":"Pick a fixture value","choices":["yes","no"]}',
                },
            )
            questions = runner.until("list_questions")
            # Deferred messages are durable but stay outside model history while
            # the current task waits for a question response.
            deferred_id = str(uuid4())
            c("queue_input", tid, {"requestId": deferred_id, "text": "After the current task", "delivery": "queue"})
            pending_inputs = c("input_queue", tid)
            assert [item["id"] for item in pending_inputs["items"]] == [deferred_id]
            pending_inputs = c("change_input_queue", tid, {"action": "edit", "revision": pending_inputs["revision"], "id": deferred_id, "text": "Edited deferred task"})
            pending_inputs = c("change_input_queue", tid, {"action": "reorder", "revision": pending_inputs["revision"], "ids": [deferred_id]})
            pending_inputs = c("change_input_queue", tid, {"action": "remove", "revision": pending_inputs["revision"], "id": deferred_id})
            assert not pending_inputs["items"]
            question = questions[0]
            pending = c("pending_question", tid)
            assert pending is not None
            qid = question.get("id", question.get("questionId"))
            c("reply_question", qid, {"answer": "yes"})
            assert (
                c("wait_turn", qturn["id"], {"timeout_ms": 10000})["status"]
                == "completed"
            )
            permission = c(
                "start_turn",
                tid,
                {
                    "requestId": str(uuid4()),
                    "text": '!tool bash {"command":"printf SDK_PERMISSION_OK"}',
                },
            )
            prompts = runner.until("list_permissions")
            pid = prompts[0].get("id", prompts[0].get("permissionId"))
            c("reply_permission", pid, {"decision": "once"})
            assert (
                c("wait_turn", permission["id"], {"timeout_ms": 10000})["status"]
                == "completed"
            )
            # Mutating tools and undo/redo prove filesystem and transcript consistency.
            c("chat", tid, '!tool write {"path":"fixture.txt","content":"after\\n"}')
            assert (fixture.workspace / "fixture.txt").read_text() == "after\n"
            assert c("last_turn_diff", tid)
            summary = c("last_turn_review", tid, {"summary": True})
            assert summary and all("before" not in f and "after" not in f for f in summary)
            detail = c("last_turn_review", tid, {"path": "fixture.txt"})
            assert len(detail) == 1 and detail[0]["after"] == "after\n"
            c("revert_thread", tid, {})
            assert (fixture.workspace / "fixture.txt").read_text() == "before\n"
            c("unrevert_thread", tid)
            assert (fixture.workspace / "fixture.txt").read_text() == "after\n"
            assert c("vcs_diff", {"mode": "working", "context": 4})
            c("vcs_diff", {"mode": "branch"})
            summary = c("vcs_diff", {"summary": True})
            assert summary and all("patch" not in f for f in summary)
            detail = c("vcs_diff", {"path": "fixture.txt", "context": 4})
            assert len(detail) == 1 and "+after" in detail[0]["patch"]
            assert "fixture.txt" in c("search_files", {"q": "fixture", "limit": 10})
            c("list_todos", tid)
            c("run_shell", tid, {"command": "printf SDK_SHELL_OK"})
            prompts = runner.until("list_permissions")
            c(
                "reply_permission",
                prompts[0].get("id", prompts[0].get("permissionId")),
                {"decision": "once"},
            )
            runner.until(
                "list_items",
                tid,
                predicate=lambda xs: any(
                    x["data"].get("output", "").find("SDK_SHELL_OK") >= 0 for x in xs
                ),
            )
            fork = c("fork_thread", tid, {"title": "Fork"})
            assert fork["parentId"] == tid
            archive = c("export_session_archive", tid, {"sanitize": False})
            assert archive["format"] == "haarness.session-archive"
            assert (
                c("import_session_archive", {"archive": archive})["root"]["id"] == tid
            )
            assert c("export_session_archive", tid, {"sanitize": True})["sanitized"]
            c("compact", fork["id"], {"summary": "SDK fixture summary"})
            c("get_goal", tid)
            c(
                "set_goal",
                tid,
                {"action": "set", "goal": "Fixture goal", "tokenBudget": 1000},
            )
            for action in ("status", "pause", "resume", "block", "clear"):
                c("set_goal", tid, {"action": action})
            c("set_loop", tid, {"prompt": "Fixture loop", "intervalSeconds": 86400})
            assert c("get_loop", tid)["loop"]["prompt"] == "Fixture loop"
            c("set_loop", tid, {"stop": True})
            c("update_tool", "webfetch", {"enabled": False})
            c("update_tool", "webfetch", {"enabled": None})
            c("update_lazy_tools", {"mode": "always"})
            c("update_lazy_tools", {"mode": None})
            assert "fixture evidence" in c("load_skill", "fixture")["instructions"]
            cmd = c(
                "execute_command",
                tid,
                {"name": "greet", "arguments": "SDK", "wait": True},
            )
            assert cmd["kind"] == "turn"
            c("wait_turn", cmd["turn"]["id"], {"timeout_ms": 10000})
            c("execute_command", tid, {"name": "workflow", "arguments": "list"})
            c("execute_command", tid, {"name": "not-a-command"}, error=404)
            # Workflows: source, output, pagination, wait, pause/resume/cancel/delete.
            assert "fixture" in c("workflow_source", "fixture")["source"]
            run = c(
                "start_workflow", {"name": "fixture", "threadId": tid, "wait": True}
            )
            assert run["status"] == "completed", run
            c("get_workflow_run", run["id"], {"afterSequence": 0})
            c("list_workflow_runs", {"threadId": tid, "limit": 10})
            c("wait_workflow_run", run["id"], {"timeoutMs": 1000})
            c("delete_workflow_run", run["id"])
            slow_source = 'export const meta = { name: "slow" }; const reply = await agent({prompt:"::mdslow",model:"echo/echo-1"}); return reply.text;'
            run = c("start_workflow", {"source": slow_source, "threadId": tid})
            c("pause_workflow_run", run["id"])
            c("resume_workflow_run", run["id"])
            c("cancel_workflow_run", run["id"])
            c("delete_workflow_run", run["id"])
            # Isolated side question.
            assert c("btw", tid, {"question": "Side question"})["answer"]
            # Cancellation closes the workflow first, then its in-flight provider turn.
            runner.until("active_turn", tid, predicate=lambda value: value is None)
            # Background child lifecycle.
            child = c(
                "start_turn",
                tid,
                {
                    "requestId": str(uuid4()),
                    "text": '!tool task {"prompt":"::mdslow","description":"Fixture child"}',
                },
            )
            runner.until("list_background_tasks", tid)
            c("promote_background_tasks", tid, {})
            c("interrupt_thread_tree", tid)
            c("wait_turn", child["id"], {"timeout_ms": 10000})
            # MCP uses a real stdio peer with tools/resources/templates/prompts.
            c(
                "configure_mcp",
                "fixture",
                {
                    "command": str(ROOT / "sdks/python/.venv/bin/python"),
                    "args": [str(ROOT / "sdks/tests/mcp.py")],
                    "transport": "stdio",
                    "enabled": True,
                },
            )
            c("reconnect_mcp", "fixture")
            tool = c("mcp_tools", "fixture")[0]["name"]
            assert tool.endswith("echo"), tool
            assert (
                c("call_mcp_tool", "fixture", tool, {"text": "MCP_OK"})["text"]
                == "MCP_OK"
            )
            assert c("mcp_resources", "fixture") and c(
                "mcp_resource_templates", "fixture"
            )
            assert "MCP_RESOURCE_OK" in json.dumps(
                c("read_mcp_resource", "fixture", {"uri": "fixture://readme"})
            )
            assert c("mcp_prompts", "fixture")
            assert "Review SDK" in json.dumps(
                c("get_mcp_prompt", "fixture", "review", {"target": "SDK"})
            )
            c("disconnect_mcp", "fixture")
            # Plugin lifecycle operates only on temporary local fixtures.
            c("plugin_action", {"action": "marketplace", "target": str(fixture.market)})
            c("plugin_action", {"action": "install", "target": str(fixture.plugin)})
            for action in ("disable", "enable", "update", "remove"):
                c("plugin_action", {"action": action, "target": "sdk-fixture"})
            # Worktree creates and destructive operations stay under the temp root.
            resolved = c("resolve_workspace_ref", {"reference": "main"})
            assert resolved["commit"]
            created = c(
                "create_workspace",
                {
                    "name": "sdk-worktree",
                    "fromRef": "main",
                    "path": str(fixture.root / "worktree"),
                },
            )
            wid = created["workspace"]["id"]
            c("get_workspace", wid)
            c("open_workspace", wid)
            c("reset_workspace", wid, {"confirm": True})
            c("remove_workspace", wid, {"confirm": True, "deleteBranch": True})
            stale = c(
                "create_workspace",
                {"name": "stale", "path": str(fixture.root / "stale")},
            )["workspace"]
            shutil.rmtree(stale["path"])
            c("cleanup_workspace", stale["id"], {"confirm": True})
            # PTY binary input/output, cursor, replay, resize, delete.
            pty = c(
                "create_pty", {"command": "/bin/sh", "args": [], "cols": 80, "rows": 24}
            )
            c("get_pty", pty["id"])
            c(
                "update_pty",
                pty["id"],
                {"title": "SDK terminal", "cols": 100, "rows": 30},
            )
            output = c("connect_pty", pty["id"], 0)
            assert "SDK_PTY_OK" in output["text"] and output["cursor"] > 0, output
            c("connect_pty", pty["id"], output["cursor"])
            c("delete_pty", pty["id"])
            # Binary profile upload/download/remove with checksum-preserving bytes.
            profile = fixture.root / "fixture.profile.zip"
            subprocess.run(
                [
                    str(BINARY),
                    "--workspace",
                    str(fixture.workspace),
                    "profile",
                    "save",
                    "fixture",
                    "--scope",
                    "local",
                    "--output",
                    str(profile),
                ],
                env=fixture.env,
                check=True,
                capture_output=True,
            )
            encoded = base64.b64encode(profile.read_bytes()).decode()
            record = c("upload_profile", encoded)
            c("list_profiles", {"query": "fixture", "offset": 0, "limit": 10})
            assert c("get_profile", record["id"])["sha256"] == record["sha256"]
            assert c("download_profile", record["id"])["base64"] == encoded
            c("delete_profile", record["id"])
            c("get_profile", record["id"], error=404)
            image = fixture.root / "image.png"
            image.write_bytes(
                base64.b64decode(
                    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="
                )
            )
            encoded = c("attachments", [str(image)])
            assert encoded[0].startswith("data:image/png;")
            c("chat", tid, "Attachment fixture", {"attachments": encoded})
            with image.open("r+b") as large:
                large.truncate(11 * 1024 * 1024)
            encoded = c("attachments", [str(image)])
            assert len(base64.b64decode(encoded[0].split(",", 1)[1])) == 11 * 1024 * 1024
            larger_chat = c("chat", tid, "Larger attachment transport fixture", {"attachments": encoded})
            assert "Echo: Larger attachment transport fixture" in larger_chat["text"], larger_chat["text"][:500]
            c("analytics_stats", {"days": 7, "limit": 100})
            assert "<html" in c("analytics_insights", {"days": 7})["html"].lower()
            c("update_thread", tid, {"archived": True})
            assert any(x["id"] == tid for x in c("list_threads", {"archived": True}))
            c("shutdown_server")
            expected = {
                x["name"]
                for x in json.loads((ROOT / "sdks/operations.json").read_text())
                if not x["path"].startswith("/v1/cloud/")  # Separate authenticated Cloud live suite.
            }
            missing = expected - runner.covered
            assert not missing, "Missing successful operation coverage: " + str(
                sorted(missing)
            )
            print(
                f"{mode}: {len(expected)}/{len(expected)} operations succeeded; lifecycle scenarios passed",
                flush=True,
            )
            return sorted(runner.covered)
        finally:
            runner.close()


if __name__ == "__main__":
    modes = sys.argv[1:] or ["typescript", "sync", "async"]
    for mode in modes:
        print("Testing " + mode, flush=True)
        scenario(mode)
