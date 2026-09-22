"""Haarness SDK: explicit server connections and owned local server helpers."""

from .client import Haarness, AsyncHaarness, ChatResult, attachments
from .server import LocalServer, AsyncLocalServer
from .transport import ApiError, TransportError, RequestOptions, ServerEvent
from .pty import PtyConnection, AsyncPtyConnection, PtyControl
from . import types

__all__ = [
    "Haarness",
    "AsyncHaarness",
    "ChatResult",
    "attachments",
    "LocalServer",
    "AsyncLocalServer",
    "ApiError",
    "TransportError",
    "RequestOptions",
    "ServerEvent",
    "PtyControl",
    "PtyConnection",
    "AsyncPtyConnection",
    "types",
]
