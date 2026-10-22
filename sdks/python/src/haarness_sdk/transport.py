"""HTTP and SSE transports. Requests are never automatically replayed."""

from __future__ import annotations

import base64
import asyncio
import json
import os
import time
from typing import Any, Iterator, AsyncIterator
from urllib.parse import quote, urlsplit
import httpx
from typing_extensions import TypedDict, Self
from .types import EventEnvelope

_MISSING = object()


class RequestOptions(TypedDict, total=False):
    timeout: float
    headers: dict[str, str]


class ApiError(Exception):
    def __init__(self, status: int, body: Any):
        record = body if isinstance(body, dict) else {}
        if isinstance(record.get("error"), dict): record = record["error"]
        self.status = status
        self.code = str(record.get("code", "http_error"))
        self.retryable = record.get("retryable") is True
        self.body = body
        super().__init__(
            str(record.get("message", record.get("error", f"HTTP {status}")))
        )


class TransportError(Exception):
    """A connection, timeout, or response-decoding failure (never an implicit retry)."""


class ResyncEvent(TypedDict, total=False):
    type: str
    schemaVersion: str


ServerEvent = EventEnvelope | ResyncEvent


def decode(response: httpx.Response, binary: bool = False, text: bool = False) -> Any:
    if not response.is_success:
        try:
            body = response.json()
        except ValueError:
            body = response.text
        raise ApiError(response.status_code, body)
    if binary:
        return response.content
    if text:
        return response.text
    if not response.content:
        return None
    try:
        return response.json()
    except ValueError as error:
        raise TransportError("Server returned invalid JSON") from error


class Base:
    def __init__(
        self,
        base_url: str,
        *,
        token: str | None = None,
        username: str | None = None,
        password: str | None = None,
        timeout: float = 30.0,
    ):
        url = urlsplit(base_url)
        if (
            url.scheme not in ("http", "https")
            or not url.hostname
            or url.username
            or url.password
            or url.query
            or url.fragment
        ):
            raise ValueError(
                "Use an HTTP(S) server URL without credentials, query or fragment"
            )
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        self.base_url = base_url.rstrip("/")
        self._token = ""  # Private local-server helpers retain their existing credential handle.
        self._authentication: dict[str, str] = {}
        if password is not None:
            user = (username or "").strip() or "haarness"
            if ":" in user or any(ord(char) < 32 or ord(char) == 127 for char in user):
                raise ValueError("Username cannot contain colons or control characters")
            if password:
                encoded = base64.b64encode(f"{user}:{password}".encode()).decode()
                self._authentication = {"Authorization": "Basic " + encoded}
        else:
            bearer = (
                token
                if token is not None
                else os.environ.get("HAARNESS_SERVER_TOKEN", "")
            )
            if "\r" in bearer or "\n" in bearer:
                raise ValueError("Invalid server token")
            self._token = bearer
            if bearer:
                self._authentication = {"Authorization": "Bearer " + bearer}
        self._timeout = timeout

    @staticmethod
    def segment(value: str) -> str:
        if not value or value in (".", ".."):
            raise ValueError("Invalid path parameter")
        return quote(value, safe="")

    def _headers(self) -> dict[str, str]:
        return dict(self._authentication)

    def _kwargs(
        self, body: Any, query: Any, binary_body: bool, options: RequestOptions | None
    ) -> dict[str, Any]:
        timeout = (options or {}).get("timeout", self._timeout)
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        kwargs: dict[str, Any] = {
            "timeout": timeout,
            "params": {
                k: str(v).lower() if isinstance(v, bool) else v
                for k, v in (query or {}).items()
                if v is not None
            },
        }
        if body is not _MISSING:
            kwargs["content"] = body if binary_body else json.dumps(body)
            kwargs["headers"] = {
                "Content-Type": "application/zip" if binary_body else "application/json"
            }
        kwargs["headers"] = {**self._headers(), **kwargs.get("headers", {}), **(options or {}).get("headers", {})}
        return kwargs


class Transport(Base):
    def __init__(
        self,
        base_url: str,
        *,
        token: str | None = None,
        username: str | None = None,
        password: str | None = None,
        timeout: float = 30.0,
    ):
        super().__init__(
            base_url, token=token, username=username, password=password, timeout=timeout
        )
        self._http = httpx.Client(
            headers=self._headers(), follow_redirects=False, trust_env=False
        )

    def request(
        self,
        method: str,
        path: str,
        *,
        body: Any = _MISSING,
        query: Any = None,
        binary_body: bool = False,
        binary_response: bool = False,
        text_response: bool = False,
        options: RequestOptions | None = None,
    ) -> Any:
        try:
            return decode(
                self._http.request(
                    method,
                    self.base_url + path,
                    **self._kwargs(body, query, binary_body, options),
                ),
                binary_response,
                text_response,
            )
        except httpx.HTTPError as error:
            raise TransportError("Haarness request failed") from error

    def close(self) -> None:
        """Release client connections; this does not shut down the server."""
        self._http.close()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *args) -> None:
        self.close()

    def _prepare_events(self) -> None:
        pass

    def _retry_events(self, error: ApiError) -> bool:
        return False

    def events(
        self,
        *,
        after: int | None = None,
        reconnect: bool = False,
        retry_seconds: float = 0.5,
        idle_timeout: float | None = None,
    ) -> Iterator[ServerEvent]:
        """Close the iterator to detach. A resync marker ends it; rehydrate REST state.

        With ``idle_timeout`` (seconds), a stream that sends nothing for that long,
        not even the keepalive a server sends every 15 s, counts as lost: a
        connection that died without closing. It connects again from its cursor
        with ``reconnect``, and otherwise ends with a ``TransportError``.
        """
        instance = None
        renewed = False
        while True:
            self._prepare_events()
            headers = {**self._headers(), "Accept": "text/event-stream"}
            if after is not None:
                headers["Last-Event-ID"] = str(after)
            try:
                with self._http.stream(
                    "GET",
                    self.base_url + "/v1/events",
                    headers=headers,
                    timeout=httpx.Timeout(self._timeout, read=idle_timeout),
                ) as response:
                    if not response.is_success:
                        response.read()
                        decode(response)
                    renewed = False
                    data: list[str] = []
                    for line in response.iter_lines():
                        if not line and data:
                            event = json.loads("\n".join(data))
                            data = []
                            if event.get("type") == "resync_required":
                                yield event
                                return
                            if instance and instance != event["serverInstanceId"]:
                                yield {"type": "resync_required"}
                                return
                            instance = event["serverInstanceId"]
                            if after is not None and event["eventId"] <= after:
                                continue
                            after = event["eventId"]
                            yield event
                        elif line.startswith("data:"):
                            data.append(line[5:].removeprefix(" "))
                            if sum(map(len, data)) > 8 * 1024 * 1024:
                                raise TransportError("Event exceeds 8 MiB")
            except ApiError as error:
                if renewed or not self._retry_events(error):
                    raise
                renewed = True
                continue
            except httpx.ReadTimeout as error:
                if not reconnect:
                    raise TransportError(
                        f"The event stream sent nothing for {idle_timeout} s"
                    ) from error
            except httpx.HTTPError as error:
                if not reconnect:
                    raise TransportError("Event stream failed") from error
            if not reconnect:
                return
            time.sleep(max(0.01, retry_seconds))


class AsyncTransport(Base):
    def __init__(
        self,
        base_url: str,
        *,
        token: str | None = None,
        username: str | None = None,
        password: str | None = None,
        timeout: float = 30.0,
    ):
        super().__init__(
            base_url, token=token, username=username, password=password, timeout=timeout
        )
        self._http = httpx.AsyncClient(
            headers=self._headers(), follow_redirects=False, trust_env=False
        )

    async def request(
        self,
        method: str,
        path: str,
        *,
        body: Any = _MISSING,
        query: Any = None,
        binary_body: bool = False,
        binary_response: bool = False,
        text_response: bool = False,
        options: RequestOptions | None = None,
    ) -> Any:
        try:
            return decode(
                await self._http.request(
                    method,
                    self.base_url + path,
                    **self._kwargs(body, query, binary_body, options),
                ),
                binary_response,
                text_response,
            )
        except httpx.HTTPError as error:
            raise TransportError("Haarness request failed") from error

    async def close(self) -> None:
        await self._http.aclose()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *args) -> None:
        await self.close()

    async def _prepare_events(self) -> None:
        pass

    async def _retry_events(self, error: ApiError) -> bool:
        return False

    async def events(
        self,
        *,
        after: int | None = None,
        reconnect: bool = False,
        retry_seconds: float = 0.5,
        idle_timeout: float | None = None,
    ) -> AsyncIterator[ServerEvent]:
        """See ``Transport.events``."""
        instance = None
        renewed = False
        while True:
            await self._prepare_events()
            headers = {**self._headers(), "Accept": "text/event-stream"}
            if after is not None:
                headers["Last-Event-ID"] = str(after)
            try:
                async with self._http.stream(
                    "GET",
                    self.base_url + "/v1/events",
                    headers=headers,
                    timeout=httpx.Timeout(self._timeout, read=idle_timeout),
                ) as response:
                    if not response.is_success:
                        await response.aread()
                        decode(response)
                    renewed = False
                    data: list[str] = []
                    async for line in response.aiter_lines():
                        if not line and data:
                            event = json.loads("\n".join(data))
                            data = []
                            if event.get("type") == "resync_required":
                                yield event
                                return
                            if instance and instance != event["serverInstanceId"]:
                                yield {"type": "resync_required"}
                                return
                            instance = event["serverInstanceId"]
                            if after is not None and event["eventId"] <= after:
                                continue
                            after = event["eventId"]
                            yield event
                        elif line.startswith("data:"):
                            data.append(line[5:].removeprefix(" "))
                            if sum(map(len, data)) > 8 * 1024 * 1024:
                                raise TransportError("Event exceeds 8 MiB")
            except ApiError as error:
                if renewed or not await self._retry_events(error):
                    raise
                renewed = True
                continue
            except httpx.ReadTimeout as error:
                if not reconnect:
                    raise TransportError(
                        f"The event stream sent nothing for {idle_timeout} s"
                    ) from error
            except httpx.HTTPError as error:
                if not reconnect:
                    raise TransportError("Event stream failed") from error
            if not reconnect:
                return
            await asyncio.sleep(max(0.01, retry_seconds))
