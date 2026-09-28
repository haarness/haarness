"""Bounded authenticated JSON WebSockets for browser hosts and Cloud connectors."""
from __future__ import annotations
import json
from typing import cast, Iterator, AsyncIterator, Callable
from typing_extensions import Self
from websockets.sync.client import connect as sync_connect
from websockets.asyncio.client import connect as async_connect, ClientConnection
from websockets.exceptions import ConnectionClosedOK
from .types import JsonValue
from .transport import TransportError

class NoRedirectConnect(async_connect):
    def process_redirect(self, exc: Exception) -> Exception | str:
        # Connector and runtime credentials must never follow a redirect.
        return exc

def decode(value: str | bytes) -> JsonValue:
    if isinstance(value, bytes): raise TransportError('Expected JSON text frame')
    try: return cast(JsonValue, json.loads(value))
    except ValueError as error: raise TransportError('Invalid JSON frame') from error

class JsonSocket:
    def __init__(self, url: str, headers: dict[str,str], timeout: float):
        self._on_close: Callable[[], None] = lambda: None
        self._socket = sync_connect(url, additional_headers=headers, proxy=None, open_timeout=timeout, max_size=24*1024*1024, max_queue=4)
    def send(self, value: JsonValue) -> None: self._socket.send(json.dumps(value))
    def recv(self, timeout: float | None = None) -> JsonValue: return decode(self._socket.recv(timeout=timeout))
    def close(self) -> None:
        self._socket.close()
        self._on_close()
    def __enter__(self) -> Self: return self
    def __exit__(self, *args) -> None: self.close()
    def __iter__(self) -> Iterator[JsonValue]:
        try:
            while True: yield self.recv()
        except ConnectionClosedOK: return

class AsyncJsonSocket:
    def __init__(self, socket: ClientConnection):
        self._socket = socket
        self._on_close: Callable[[], None] = lambda: None
    @classmethod
    async def connect(cls, url: str, headers: dict[str,str], timeout: float) -> Self:
        return cls(await NoRedirectConnect(url, additional_headers=headers, proxy=None, open_timeout=timeout, max_size=24*1024*1024, max_queue=4))
    async def send(self, value: JsonValue) -> None: await self._socket.send(json.dumps(value))
    async def recv(self) -> JsonValue: return decode(await self._socket.recv())
    async def close(self) -> None:
        await self._socket.close()
        self._on_close()
    async def __aenter__(self) -> Self: return self
    async def __aexit__(self, *args) -> None: await self.close()
    async def __aiter__(self) -> AsyncIterator[JsonValue]:
        try:
            while True: yield await self.recv()
        except ConnectionClosedOK: return
