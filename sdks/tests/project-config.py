"""Project config defaults and opt-out through both Python server owners."""
import asyncio
import os
import tempfile
from pathlib import Path
from haarness_sdk import LocalServer, AsyncLocalServer

with tempfile.TemporaryDirectory(prefix="haarness-python-project-config-") as directory:
    root = Path(directory)
    workspace = root / "project"
    state = root / "state"
    for path in [workspace / ".haarness/commands", state / "config/commands"]:
        path.mkdir(parents=True)
    (workspace / ".haarness/commands/project-proof.md").write_text("Project command")
    (state / "config/commands/global-proof.md").write_text("Global command")
    common = {"workspace": str(workspace), "data_dir": str(state),
              "executable": str(Path(os.environ.get("HAARNESS_BIN", "target/debug/haarness")).resolve())}
    cases = [("default", {}, True), ("opt-out", {"disable_project_config": True}, False),
             ("legacy false", {"trust_project": False}, False),
             ("legacy true", {"trust_project": True}, True),
             ("environment opt-out", {"env": {"HAARNESS_DISABLE_PROJECT_CONFIG":"1"}}, False)]

    def parameters(options):
        return {**common, **options, "env": {"HOME": str(root), "HAARNESS_HOME": str(root),
                "HAARNESS_TEST_PROVIDER":"1", "HAARNESS_DISABLE_PROJECT_CONFIG":"0", **options.get("env", {})}}

    for label, options, enabled in cases:
        with LocalServer.start(**parameters(options)) as server:
            assert server.client.info()["projectTrusted"] is enabled, label
            commands = {item["name"] for item in server.client.commands()}
            assert ("project-proof" in commands) is enabled, label
            assert "global-proof" in commands
            print("PASS Python SDK", label, flush=True)

    async def verify_async():
        for label, options, enabled in cases:
            async with await AsyncLocalServer.start(**parameters(options)) as server:
                assert (await server.client.info())["projectTrusted"] is enabled, label
                commands = {item["name"] for item in await server.client.commands()}
                assert ("project-proof" in commands) is enabled, label
                assert "global-proof" in commands
                print("PASS async Python SDK", label, flush=True)
    asyncio.run(verify_async())
