#!/usr/bin/env python3
"""Real Cloud + gateway + native runtime SDK journey, on an isolated fixture stack."""

import asyncio
import base64
import inspect
import json
import os
import sys
import time
from datetime import datetime, timezone, timedelta
from pathlib import Path
from uuid import uuid4
from haarness_sdk import HaarnessCloud, AsyncHaarnessCloud, ApiError
from cloud_fixture import authorize, fund

BASE = os.environ.get("HAARNESS_SDK_CLOUD_URL", "http://127.0.0.1:28080")
EMPTY = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"


async def call(method, *args, **kwargs):
    value = method(*args, **kwargs)
    return await value if inspect.isawaitable(value) else value


async def poll(fn, check, budget=90):
    end = time.monotonic() + budget
    while time.monotonic() < end:
        value = await fn()
        if check(value):
            return value
        await asyncio.sleep(0.5)
    raise AssertionError(f"Timed out: {value}")


async def scenario(asynchronous):
    label = "async" if asynchronous else "sync"
    tokens = authorize(BASE, "sdk-" + label + "-" + uuid4().hex)
    with HaarnessCloud(BASE, token=tokens["access_token"]) as admin:
        fund(admin)
    cloud = (AsyncHaarnessCloud if asynchronous else HaarnessCloud)(
        BASE, token=tokens["access_token"]
    )
    session = None
    try:
        session = await call(
            cloud.create_session,
            {
                "title": "SDK " + label + " shared session",
                "configuration": {"model": "router/fixture/echo", "diskGb": 1},
                "start": True,
            },
        )
        await poll(
            lambda: call(session.get),
            lambda s: s["workspace"]["observedState"] == "running",
        )
        a = await call(session.chats.create, {"title": "first chat"})
        b = await call(session.chats.create, {"title": "second chat"})
        assert len(await call(session.chats.list)) == 2
        await call(b.rename, "renamed second chat")
        assert (await call(b.get))["title"] == "renamed second chat"
        runtime = await call(session.connect)
        await call(runtime.mutate_file, {"action": "mkdir", "path": "sdk-files"})
        payload = "shared file 雪\nSDK content " + label
        saved = await call(
            runtime.write_file,
            {"path": "sdk-files/upload.txt", "content": payload, "revision": EMPTY},
        )
        assert (await call(runtime.read_file, {"path": "sdk-files/upload.txt"}))[
            "content"
        ] == payload
        assert (
            len((await call(runtime.list_files, {"path": "sdk-files"}))["entries"]) == 1
        )
        binary = base64.b64encode(bytes(range(256))).decode()
        await call(
            runtime.write_file,
            {
                "path": "sdk-files/binary.dat",
                "content": binary,
                "encoding": "base64",
                "revision": EMPTY,
            },
        )
        assert (await call(runtime.read_file, {"path": "sdk-files/binary.dat"}))[
            "content"
        ] == binary
        try:
            await call(
                runtime.write_file,
                {
                    "path": "sdk-files/upload.txt",
                    "content": "stale overwrite",
                    "revision": EMPTY,
                },
            )
        except ApiError as e:
            assert e.status == 409
        else:
            raise AssertionError("Must reject a stale upload overwrite")
        await call(
            runtime.mutate_file,
            {
                "action": "rename",
                "path": "sdk-files/upload.txt",
                "target": "sdk-files/renamed.txt",
            },
        )
        result = await call(a.ask, "hello fixture", {"tools": {}})
        assert result["turn"]["status"] == "completed"
        try:
            await call(b.wait, result["turn"]["id"])
        except ValueError:
            pass
        else:
            raise AssertionError("Cross-chat turn must be rejected")
        assert not await call(b.messages)
        sid = session.id
        cid = a.id
        key = {"headers": {"Idempotency-Key": uuid4().hex}}
        schedule = await call(
            cloud.create_cloud_schedule,
            {
                "sessionId": sid,
                "chatId": cid,
                "name": "SDK timed run",
                "prompt": "scheduled hello",
                "at": (datetime.now(timezone.utc) + timedelta(seconds=4)).isoformat(),
                "everySeconds": 60,
            },
            options=key,
        )
        schedule_id = schedule["id"]
        assert (await call(cloud.list_cloud_schedules, {"sessionId": sid}))["items"][0][
            "id"
        ] == schedule_id
        await poll(
            lambda: call(cloud.cloud_schedule_runs, schedule_id),
            lambda p: p["items"] and p["items"][0]["status"] == "completed",
        )
        fresh = await call(cloud.get_cloud_schedule, schedule_id)
        paused = await call(
            cloud.update_cloud_schedule,
            schedule_id,
            {"version": fresh["version"], "enabled": False},
        )
        assert not paused["enabled"]
        try:
            await call(
                cloud.update_cloud_schedule,
                schedule_id,
                {"version": fresh["version"], "enabled": True},
            )
        except ApiError as e:
            assert e.status == 409
        else:
            raise AssertionError("Version guard absent")
        run_key = {"headers": {"Idempotency-Key": uuid4().hex}}
        now = await call(cloud.run_cloud_schedule, schedule_id, options=run_key)
        assert (await call(cloud.run_cloud_schedule, schedule_id, options=run_key))[
            "id"
        ] == now["id"]
        await poll(
            lambda: call(cloud.cloud_schedule_runs, schedule_id),
            lambda p: (
                next(r for r in p["items"] if r["id"] == now["id"])["status"]
                == "completed"
            ),
        )
        await call(session.stop)
        await poll(
            lambda: call(session.get),
            lambda s: s["workspace"]["observedState"] == "stopped",
        )
        # Server-side scheduler wakes compute while the SDK has no runtime connection.
        await call(session.close)
        fresh = await call(cloud.get_cloud_schedule, schedule_id)
        await call(
            cloud.update_cloud_schedule,
            schedule_id,
            {
                "version": fresh["version"],
                "enabled": True,
                "at": (datetime.now(timezone.utc) + timedelta(seconds=4)).isoformat(),
            },
        )
        history = await poll(
            lambda: call(cloud.cloud_schedule_runs, schedule_id),
            lambda p: len(p["items"]) == 3 and p["items"][0]["status"] == "completed",
        )
        runtime = await call(session.connect)
        assert len(await call(session.chats.list)) == 2
        assert (await call(runtime.read_file, {"path": "sdk-files/renamed.txt"}))[
            "content"
        ] == payload
        assert all(r["chatId"] == cid for r in history["items"])
        # Archived chats disable repeating schedules; no unwanted messages or continued wakeups.
        await call(a.archive)
        failed = await call(
            cloud.run_cloud_schedule,
            schedule_id,
            options={"headers": {"Idempotency-Key": uuid4().hex}},
        )
        await poll(
            lambda: call(cloud.cloud_schedule_runs, schedule_id),
            lambda p: (
                next(r for r in p["items"] if r["id"] == failed["id"])["status"]
                == "failed"
            ),
        )
        assert not (await call(cloud.get_cloud_schedule, schedule_id))["enabled"]
        await call(cloud.delete_cloud_schedule, schedule_id)
        try:
            await call(cloud.get_cloud_schedule, schedule_id)
        except ApiError as e:
            assert e.status == 404
        else:
            raise AssertionError("Deleted schedule visible")
        await call(
            runtime.mutate_file,
            {"action": "delete", "path": "sdk-files/renamed.txt", "confirm": True},
        )
        await call(
            runtime.mutate_file,
            {"action": "delete", "path": "sdk-files/binary.dat", "confirm": True},
        )
        await call(
            runtime.mutate_file,
            {"action": "delete", "path": "sdk-files", "confirm": True},
        )
        await call(b.archive)
        print(
            f"{label}: sessions, two chats, binary/text uploads, conflict/rename/delete, 7 schedule endpoints, timed run, stop/wake/persistence, archive safety PASS",
            flush=True,
        )
    finally:
        if session:
            try:
                deletion = await call(session.delete)
                final = await poll(
                    lambda: call(cloud.get_operation, deletion["operation"]["id"]),
                    lambda op: op["status"] in ("succeeded", "failed", "cancelled"),
                )
                assert final["status"] == "succeeded", final
            finally:
                await call(session.close)
        await call(cloud.close)


if __name__ == "__main__":
    asyncio.run(scenario(sys.argv[1:] == ["async"]))
