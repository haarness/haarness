"""Haarness SDK: explicit server connections and owned local server helpers."""

from .client import Haarness, AsyncHaarness, ChatResult, NEW_FILE_REVISION, attachments
from .server import LocalServer, AsyncLocalServer
from .transport import ApiError, TransportError, RequestOptions, ServerEvent
from .pty import PtyConnection, AsyncPtyConnection, PtyControl
from . import types

__all__ = [
    "Haarness",
    "AsyncHaarness",
    "ChatResult",
    "attachments",
    "NEW_FILE_REVISION",
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

from .cloud import HaarnessCloud, AsyncHaarnessCloud, CloudWorkspace, AsyncCloudWorkspace, CloudChat, AsyncCloudChat
from . import cloud_types
__all__ += ["HaarnessCloud", "AsyncHaarnessCloud", "CloudWorkspace", "AsyncCloudWorkspace", "CloudChat", "AsyncCloudChat", "cloud_types"]

from .router import HaarnessRouter, AsyncHaarnessRouter, RouterChatRequest
from .json_socket import JsonSocket, AsyncJsonSocket
__all__ += ["HaarnessRouter", "AsyncHaarnessRouter", "RouterChatRequest", "JsonSocket", "AsyncJsonSocket"]
