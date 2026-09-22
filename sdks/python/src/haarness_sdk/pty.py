"""Authenticated PTY connections. Binary output and JSON cursor/replay controls."""

from __future__ import annotations
import json
from typing import Iterator, AsyncIterator, Literal, cast
from typing_extensions import Self, TypedDict
from websockets.sync.client import connect as sync_connect
from websockets.asyncio.client import connect as async_connect, ClientConnection
from websockets.exceptions import ConnectionClosedOK
from .transport import TransportError


class PtyControl(TypedDict):
    type: Literal["replay", "cursor"]
    start: int
    end: int
    truncated: bool


class PtyConnection:
    def __init__(self, url: str, headers: dict[str, str], timeout: float):
        try:
            self._socket = sync_connect(
                url,
                additional_headers=headers,
                proxy=None,
                open_timeout=timeout,
                max_size=8 * 1024 * 1024,
            )
        except Exception as error:
            raise TransportError("PTY connection failed") from error
        self.cursor = 0

    def send(self, data: str | bytes) -> None:
        self._socket.send(data.encode() if isinstance(data, str) else data)

    def recv(self, timeout: float | None = None) -> bytes | PtyControl:
        data = self._socket.recv(timeout=timeout)
        if isinstance(data, bytes):
            return data
        message = cast(PtyControl, json.loads(data))
        if message.get("type") == "cursor" and isinstance(message.get("end"), int):
            self.cursor = message["end"]
        return message

    def __iter__(self) -> Iterator[bytes | PtyControl]:
        try:
            while True:
                yield self.recv()
        except ConnectionClosedOK:
            return

    def close(self) -> None:
        self._socket.close()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *args) -> None:
        self.close()


class AsyncPtyConnection:
    def __init__(self, socket: ClientConnection):
        self._socket = socket
        self.cursor = 0

    @classmethod
    async def connect(cls, url: str, headers: dict[str, str], timeout: float) -> Self:
        try:
            return cls(
                await async_connect(
                    url,
                    additional_headers=headers,
                    proxy=None,
                    open_timeout=timeout,
                    max_size=8 * 1024 * 1024,
                )
            )
        except Exception as error:
            raise TransportError("PTY connection failed") from error

    async def send(self, data: str | bytes) -> None:
        await self._socket.send(data.encode() if isinstance(data, str) else data)

    async def recv(self) -> bytes | PtyControl:
        data = await self._socket.recv()
        if isinstance(data, bytes):
            return data
        message = cast(PtyControl, json.loads(data))
        if message.get("type") == "cursor" and isinstance(message.get("end"), int):
            self.cursor = message["end"]
        return message

    async def __aiter__(self) -> AsyncIterator[bytes | PtyControl]:
        try:
            while True:
                yield await self.recv()
        except ConnectionClosedOK:
            return

    async def close(self) -> None:
        await self._socket.close()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *args) -> None:
        await self.close()
