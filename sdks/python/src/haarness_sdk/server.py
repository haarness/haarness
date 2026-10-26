"""Explicit ownership of local server processes. No implicit daemon replacement."""

from __future__ import annotations
import asyncio
import os
import collections
import queue
import re
import secrets
import subprocess
import threading
from typing import Mapping
from typing_extensions import Self
from .client import Haarness, AsyncHaarness


class LocalServer:
    def __init__(self, client: Haarness, process: subprocess.Popen[str]):
        self.client = client
        self._process = process
        self._closed = False

    @classmethod
    def start(
        cls,
        workspace: str,
        *,
        executable: str = "haarness",
        data_dir: str | None = None,
        env: Mapping[str, str] | None = None,
        disable_project_config: bool = False,
        trust_project: bool | None = None,
        yolo: bool = False,
        startup_timeout: float = 30,
    ) -> Self:
        token = secrets.token_hex(32)
        args = (
            [executable, "--workspace", workspace]
            + (["--disable-project-config"] if disable_project_config or trust_project is False else [])
            + (["--yolo"] if yolo else [])
            + ["serve", "--port", "0"]
        )
        environment = {**os.environ, **(env or {}), "HAARNESS_SERVER_TOKEN": token}
        if data_dir:
            environment["HAARNESS_DATA_DIR"] = data_dir
        process = subprocess.Popen(
            args,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        ready: queue.Queue[str | None] = queue.Queue()
        # What the server last said on stderr, kept (and the pipe drained) for as long as it runs: a failed
        # start reports why, such as another server already running in that folder.
        said: collections.deque[str] = collections.deque(maxlen=20)

        def drain():
            assert process.stderr
            for line in process.stderr:
                if line.strip():
                    said.append(line.strip())

        errors = threading.Thread(target=drain, daemon=True)
        errors.start()

        def failure(what: str) -> RuntimeError:
            errors.join(timeout=1)
            return RuntimeError(f"{what}: {said[-1]}" if said else what)

        def read():
            assert process.stdout
            for line in process.stdout:
                match = re.search(r"Haarness server listening on (http://\S+)", line)
                if match:
                    ready.put(match[1])
            ready.put(None)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        client = None
        try:
            try:
                url = ready.get(timeout=startup_timeout)
            except queue.Empty:
                raise failure("Haarness startup timed out") from None
            if not url:
                raise failure("Haarness failed to start")
            client = Haarness(url, token=token)
            info = client.info()
            if info["name"] != "haarness" or info["schemaVersion"] != "1":
                raise RuntimeError("Incompatible Haarness server")
            return cls(client, process)
        except BaseException:
            if client:
                client.close()
            cls._terminate(process)
            raise

    @staticmethod
    def _terminate(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if process.stdout:
            process.stdout.close()
        if process.stderr:
            process.stderr.close()

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            if self._process.poll() is None:
                self.client.shutdown_server(options={"timeout": 3})
        except Exception:
            pass
        finally:
            self.client.close()
            self._terminate(self._process)

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *args) -> None:
        self.close()


class AsyncLocalServer:
    def __init__(self, owner: LocalServer):
        self._owner = owner
        self.client = AsyncHaarness(owner.client.base_url, token=owner.client._token)

    @classmethod
    async def start(
        cls,
        workspace: str,
        *,
        executable: str = "haarness",
        data_dir: str | None = None,
        env: Mapping[str, str] | None = None,
        disable_project_config: bool = False,
        trust_project: bool | None = None,
        yolo: bool = False,
        startup_timeout: float = 30,
    ) -> Self:
        # shield startup so cancellation can still reap an owned child.
        task = asyncio.create_task(
            asyncio.to_thread(
                LocalServer.start,
                workspace,
                executable=executable,
                data_dir=data_dir,
                env=env,
                trust_project=trust_project,
                disable_project_config=disable_project_config,
                yolo=yolo,
                startup_timeout=startup_timeout,
            )
        )
        try:
            return cls(await asyncio.shield(task))
        except asyncio.CancelledError:
            owner = await task
            await asyncio.to_thread(owner.close)
            raise

    async def close(self) -> None:
        await self.client.close()
        await asyncio.to_thread(self._owner.close)

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *args) -> None:
        await self.close()
