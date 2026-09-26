"""Account HTTP and WebSocket transports; no implicit mutation retries."""

from __future__ import annotations
import asyncio
import json
import os
import threading
from typing import Any, Callable, Iterator, AsyncIterator
from urllib.parse import urlencode
import httpx
from websockets.sync.client import connect
from websockets.asyncio.client import connect as WebSocketConnect
from .transport import (
    Transport as RuntimeTransport,
    AsyncTransport as AsyncRuntimeTransport,
    RequestOptions,
    TransportError,
    decode,
    _MISSING,
)


class NoRedirectConnect(WebSocketConnect):
    """Never forward account credentials to a WebSocket redirect target."""

    def process_redirect(self, exc: Exception) -> Exception:
        return exc


def response_value(
    response: httpx.Response, binary: bool, text: bool, redirect: bool
) -> Any:
    if redirect and response.is_redirect:
        return {
            "status": response.status_code,
            "location": response.headers.get("location"),
        }
    if text and response.is_success:
        return response.text
    return decode(response, binary)


class CloudBase:
    _token_resolver: Callable[[], str] | None
    _account_id: str | None
    _sockets: set[Any]
    _closed: threading.Event

    def cloud_init(self, account_id, resolve_token):
        self._account_id = account_id
        self._token_resolver = resolve_token
        self._sockets = set()
        self._closed = threading.Event()

    def cloud_options(self, body, content_type, options):
        headers = dict((options or {}).get("headers", {}))
        if self._closed.is_set():
            raise TransportError("Cloud client is closed")
        if self._token_resolver:
            token = self._token_resolver()
            if "\r" in token or "\n" in token:
                raise ValueError("Invalid token")
            headers["Authorization"] = "Bearer " + token
        if content_type:
            headers["Content-Type"] = content_type
        if content_type == "application/x-www-form-urlencoded":
            body = urlencode(body).encode()
        elif content_type == "text/plain":
            body = str(body).encode()
        return body, {**(options or {}), "headers": headers}


class Transport(RuntimeTransport, CloudBase):
    def __init__(
        self,
        base_url: str,
        *,
        token: str | None = None,
        account_id: str | None = None,
        resolve_token: Callable[[], str] | None = None,
        timeout: float = 30,
    ):
        super().__init__(
            base_url,
            token=token if token is not None else os.getenv("HAARNESS_CLOUD_TOKEN", ""),
            timeout=timeout,
        )
        self.cloud_init(account_id, resolve_token)

    def request(
        self,
        method: str,
        path: str,
        *,
        body: Any = _MISSING,
        query: Any = None,
        binary_body: bool = False,
        binary_response: bool = False,
        content_type: str | None = None,
        text_response: bool = False,
        redirect_response: bool = False,
        options: RequestOptions | None = None,
    ) -> Any:
        body, options = self.cloud_options(body, content_type, options)
        try:
            response = self._http.request(
                method,
                self.base_url + path,
                **self._kwargs(
                    body,
                    query,
                    binary_body
                    or content_type
                    in ("application/x-www-form-urlencoded", "text/plain"),
                    options,
                ),
            )
            return response_value(
                response, binary_response, text_response, redirect_response
            )
        except httpx.HTTPError as error:
            raise TransportError("Cloud request failed") from error

    def events(
        self,
        *,
        account_id: str | None = None,
        reconnect: bool = False,
        retry_seconds: float = 0.5,
        **kwargs: Any,
    ) -> Iterator[Any]:
        delay = retry_seconds
        while not self._closed.is_set():
            _, opts = self.cloud_options(_MISSING, None, None)
            query = (
                urlencode({"accountId": account_id or self._account_id})
                if account_id or self._account_id
                else ""
            )
            socket = None
            try:
                with connect(
                    self.base_url.replace("http", "ws", 1)
                    + "/api/v1/events"
                    + ("?" + query if query else ""),
                    additional_headers={**self._headers(), **opts["headers"]},
                    open_timeout=self._timeout,
                    max_size=64 * 1024,
                    max_queue=256,
                    proxy=None,
                ) as socket:
                    self._sockets.add(socket)
                    while not self._closed.is_set():
                        raw = socket.recv(timeout=45)
                        event = json.loads(raw)
                        if not isinstance(event, dict) or not isinstance(
                            event.get("type"), str
                        ):
                            raise TransportError("Invalid Cloud event")
                        delay = retry_seconds
                        yield event
            except Exception:
                if self._closed.is_set():
                    return
                if not reconnect:
                    raise
                yield {"type": "resync_required"}
            finally:
                if socket is not None:
                    self._sockets.discard(socket)
            if not reconnect:
                return
            if self._closed.wait(delay):
                return
            delay = min(delay * 2, 30)

    def close(self) -> None:
        self._closed.set()
        for socket in list(self._sockets):
            socket.close()
        super().close()


class AsyncTransport(AsyncRuntimeTransport, CloudBase):
    def __init__(
        self,
        base_url: str,
        *,
        token: str | None = None,
        account_id: str | None = None,
        resolve_token: Callable[[], str] | None = None,
        timeout: float = 30,
    ):
        super().__init__(
            base_url,
            token=token if token is not None else os.getenv("HAARNESS_CLOUD_TOKEN", ""),
            timeout=timeout,
        )
        self.cloud_init(account_id, resolve_token)
        self._async_closed = asyncio.Event()

    async def request(
        self,
        method: str,
        path: str,
        *,
        body: Any = _MISSING,
        query: Any = None,
        binary_body: bool = False,
        binary_response: bool = False,
        content_type: str | None = None,
        text_response: bool = False,
        redirect_response: bool = False,
        options: RequestOptions | None = None,
    ) -> Any:
        body, options = self.cloud_options(body, content_type, options)
        try:
            response = await self._http.request(
                method,
                self.base_url + path,
                **self._kwargs(
                    body,
                    query,
                    binary_body
                    or content_type
                    in ("application/x-www-form-urlencoded", "text/plain"),
                    options,
                ),
            )
            return response_value(
                response, binary_response, text_response, redirect_response
            )
        except httpx.HTTPError as error:
            raise TransportError("Cloud request failed") from error

    async def events(
        self,
        *,
        account_id: str | None = None,
        reconnect: bool = False,
        retry_seconds: float = 0.5,
        **kwargs: Any,
    ) -> AsyncIterator[Any]:
        delay = retry_seconds
        while not self._closed.is_set():
            _, opts = self.cloud_options(_MISSING, None, None)
            query = (
                urlencode({"accountId": account_id or self._account_id})
                if account_id or self._account_id
                else ""
            )
            socket = None
            try:
                async with NoRedirectConnect(
                    self.base_url.replace("http", "ws", 1)
                    + "/api/v1/events"
                    + ("?" + query if query else ""),
                    additional_headers={**self._headers(), **opts["headers"]},
                    open_timeout=self._timeout,
                    max_size=64 * 1024,
                    max_queue=256,
                    proxy=None,
                ) as socket:
                    self._sockets.add(socket)
                    while not self._closed.is_set():
                        raw = await asyncio.wait_for(socket.recv(), 45)
                        event = json.loads(raw)
                        if not isinstance(event, dict) or not isinstance(
                            event.get("type"), str
                        ):
                            raise TransportError("Invalid Cloud event")
                        delay = retry_seconds
                        yield event
            except Exception:
                if self._closed.is_set():
                    return
                if not reconnect:
                    raise
                yield {"type": "resync_required"}
            finally:
                if socket is not None:
                    self._sockets.discard(socket)
            if not reconnect:
                return
            try:
                await asyncio.wait_for(self._async_closed.wait(), delay)
                return
            except asyncio.TimeoutError:
                pass
            delay = min(delay * 2, 30)

    async def close(self) -> None:
        self._closed.set()
        self._async_closed.set()
        for socket in list(self._sockets):
            await socket.close()
        await super().close()
