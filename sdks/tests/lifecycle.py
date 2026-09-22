"""Exercise owned local servers, cancellation, streaming and failure cleanup."""

import asyncio
import subprocess
from uuid import uuid4
from server import Fixture, ROOT, BINARY
from haarness_sdk import (
    LocalServer,
    AsyncLocalServer,
    Haarness,
    AsyncHaarness,
    TransportError,
)

with Fixture() as fixture:
    # Real in-flight timeout detaches the wait; it does not interrupt server work.
    with Haarness(fixture.url, token=fixture.token) as client:
        thread = client.create_thread({"provider": "echo", "model": "echo-1"})
        turn = client.start_turn(
            thread["id"], {"requestId": str(uuid4()), "text": "::mdslow"}
        )
        try:
            client.wait_turn(
                turn["id"], {"timeout_ms": 10000}, options={"timeout": 0.01}
            )
        except TransportError:
            pass
        else:
            raise AssertionError("HTTP timeout did not fire")
        assert (
            client.wait_turn(turn["id"], {"timeout_ms": 10000})["status"] == "completed"
        )

    async def live():
        async with AsyncHaarness(fixture.url, token=fixture.token) as client:
            thread = await client.create_thread({"provider": "echo", "model": "echo-1"})
            cursor = (await client.info())["eventCursor"]

            async def observe():
                stream = client.events(after=cursor)
                try:
                    async for event in stream:
                        if (
                            event.get("turnId")
                            and event["type"] == "turn.finished"
                            and event.get("payload", {}).get("status") == "completed"
                        ):
                            return
                finally:
                    await stream.aclose()

            observer = asyncio.create_task(observe())
            turn = await client.start_turn(
                thread["id"], {"requestId": str(uuid4()), "text": "::mdslow"}
            )
            waiting = asyncio.create_task(
                client.wait_turn(turn["id"], {"timeout_ms": 10000})
            )
            await asyncio.sleep(0.03)
            waiting.cancel()
            try:
                await waiting
            except asyncio.CancelledError:
                pass
            await asyncio.wait_for(observer, 10)
            assert (await client.get_turn(turn["id"]))["status"] == "completed"

    asyncio.run(live())
    # Standalone TypeScript lifecycle test uses its own private workspace/data.
    path = fixture.root / "typescript-workspace"
    path.mkdir()
    subprocess.run(
        [
            "node",
            str(ROOT / "sdks/tests/lifecycle.mjs"),
            str(BINARY),
            str(path),
            str(fixture.root / "typescript-state"),
        ],
        env=fixture.env,
        check=True,
    )

fixture = Fixture()
try:
    with LocalServer.start(
        str(fixture.workspace),
        executable=str(BINARY),
        data_dir=str(fixture.data),
        env=fixture.env,
        trust_project=True,
    ) as server:
        thread = server.client.create_thread({"provider": "echo", "model": "echo-1"})
        assert (
            server.client.chat(thread["id"], "Owned server")["text"]
            == "Echo: Owned server"
        )
        pid = server._process.pid
        # Ownership conflict must not stop the already-running server.
        try:
            LocalServer.start(
                str(fixture.workspace),
                executable=str(BINARY),
                data_dir=str(fixture.data),
                env=fixture.env,
                startup_timeout=5,
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("Unexpected duplicate server")
        assert server.client.info()["name"] == "haarness"
    assert server._process.poll() is not None

    async def owned():
        async with await AsyncLocalServer.start(
            str(fixture.workspace),
            executable=str(BINARY),
            data_dir=str(fixture.data),
            env=fixture.env,
            trust_project=True,
        ) as server:
            thread = await server.client.create_thread(
                {"provider": "echo", "model": "echo-1"}
            )
            assert (await server.client.chat(thread["id"], "Async owned"))[
                "text"
            ] == "Echo: Async owned"
        assert server._owner._process.poll() is not None

    asyncio.run(owned())
finally:
    fixture.close()
print("SDK owned-server, live-stream, timeout and cancellation checks passed")
