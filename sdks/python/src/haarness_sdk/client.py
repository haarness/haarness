"""Convenience clients over the complete generated API."""

from __future__ import annotations
import base64
from pathlib import Path
from urllib.parse import quote
from uuid import uuid4
from typing_extensions import TypedDict
from .api import Api, AsyncApi
from .types import StartTurnRequest, Turn, Item, ChatOptions, JsonValue
from .transport import RequestOptions
from .json_socket import JsonSocket, AsyncJsonSocket
from .pty import PtyConnection, AsyncPtyConnection


class ChatResult(TypedDict):
    turn: Turn
    items: list[Item]
    text: str


def result(turn: Turn, items: list[Item]) -> ChatResult:
    selected = [item for item in items if item.get("turnId") == turn["id"]]
    return {
        "turn": turn,
        "items": selected,
        "text": "".join(
            item["data"]["text"]
            for item in selected
            if item["data"]["type"] == "assistant_message"
        ),
    }


class Haarness(Api):
    def capabilities(self, input: dict[str, JsonValue] | None = None, *, options: RequestOptions | None = None) -> JsonValue:
        """Manage installed runtime capabilities; changes persist and apply when idle."""
        return self.request("POST", "/v1/manage/capabilities", body=input or {"action": "list"}, options={"timeout": 180.0, **(options or {})})

    def marketplace(self, input: dict[str, JsonValue], *, options: RequestOptions | None = None) -> JsonValue:
        """Browse, install, update, enable, disable and remove marketplace resources."""
        return self.request("POST", "/v1/manage/marketplace", body=input, options={"timeout": 180.0, **(options or {})})

    def workspace(self, id: str) -> Haarness:
        """Bind the complete runtime API to an opaque host workspace ID."""
        parent = self
        class WorkspaceRuntime(Haarness):
            def _headers(self) -> dict[str, str]:
                return parent._headers()
        return WorkspaceRuntime(self.base_url + '/v1/host/workspaces/' + self.segment(id), token='', timeout=self._timeout)

    def chat(
        self,
        id: str,
        text: str,
        body: ChatOptions | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> ChatResult:
        request: StartTurnRequest = {"requestId": str(uuid4()), "text": text}
        request.update(body or {})
        request["text"] = text
        turn = self.start_turn(id, request, options=options)
        turn = self.wait_turn(
            turn["id"], {"timeout_ms": 300000}, options=options or {"timeout": 305}
        )
        return result(turn, self.list_items(id, options=options))

    def connect_browser_host(self, id: str, *, version: int = 2, timeout: float | None = None) -> JsonSocket:
        return JsonSocket(self.base_url.replace("http", "ws", 1) + "/v1/threads/" + self.segment(id) + "/browser/host?version=" + str(version), self._headers(), timeout or self._timeout)

    def connect_browser_extension_host(self, *, version: int = 1, timeout: float | None = None) -> JsonSocket:
        """Haarness for Chrome's bridge: one Chrome profile serving every chat. Send ``hello`` first."""
        return JsonSocket(self.base_url.replace("http", "ws", 1) + "/v1/browser/extension/host?version=" + str(version), self._headers(), timeout or self._timeout)

    def connect_draft_browser_host(self, id: str, *, timeout: float | None = None) -> JsonSocket:
        return JsonSocket(self.base_url.replace("http", "ws", 1) + "/v1/browser/drafts/" + self.segment(id) + "/host", self._headers(), timeout or self._timeout)

    def connect_pty(
        self, id: str, *, cursor: int = 0, timeout: float | None = None
    ) -> PtyConnection:
        return PtyConnection(
            self.base_url.replace("http", "ws", 1)
            + "/v1/ptys/"
            + self.segment(id)
            + "/connect?cursor="
            + str(cursor),
            self._headers(),
            timeout or self._timeout,
        )


class AsyncHaarness(AsyncApi):
    async def capabilities(self, input: dict[str, JsonValue] | None = None, *, options: RequestOptions | None = None) -> JsonValue:
        """Manage persistent capability availability on this runtime."""
        return await self.request("POST", "/v1/manage/capabilities", body=input or {"action": "list"}, options={"timeout": 180.0, **(options or {})})

    async def marketplace(self, input: dict[str, JsonValue], *, options: RequestOptions | None = None) -> JsonValue:
        """Use the same marketplace operations as CLI, tools, Desktop and Cloud."""
        return await self.request("POST", "/v1/manage/marketplace", body=input, options={"timeout": 180.0, **(options or {})})

    def workspace(self, id: str) -> AsyncHaarness:
        parent = self
        class WorkspaceRuntime(AsyncHaarness):
            def _headers(self) -> dict[str, str]:
                return parent._headers()
        return WorkspaceRuntime(self.base_url + '/v1/host/workspaces/' + self.segment(id), token='', timeout=self._timeout)

    async def chat(
        self,
        id: str,
        text: str,
        body: ChatOptions | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> ChatResult:
        request: StartTurnRequest = {"requestId": str(uuid4()), "text": text}
        request.update(body or {})
        request["text"] = text
        turn = await self.start_turn(id, request, options=options)
        turn = await self.wait_turn(
            turn["id"], {"timeout_ms": 300000}, options=options or {"timeout": 305}
        )
        return result(turn, await self.list_items(id, options=options))

    async def connect_browser_host(self, id: str, *, version: int = 2, timeout: float | None = None) -> AsyncJsonSocket:
        return await AsyncJsonSocket.connect(self.base_url.replace("http", "ws", 1) + "/v1/threads/" + self.segment(id) + "/browser/host?version=" + str(version), self._headers(), timeout or self._timeout)

    async def connect_browser_extension_host(self, *, version: int = 1, timeout: float | None = None) -> AsyncJsonSocket:
        """Haarness for Chrome's bridge: one Chrome profile serving every chat. Send ``hello`` first."""
        return await AsyncJsonSocket.connect(self.base_url.replace("http", "ws", 1) + "/v1/browser/extension/host?version=" + str(version), self._headers(), timeout or self._timeout)

    async def connect_draft_browser_host(self, id: str, *, timeout: float | None = None) -> AsyncJsonSocket:
        return await AsyncJsonSocket.connect(self.base_url.replace("http", "ws", 1) + "/v1/browser/drafts/" + self.segment(id) + "/host", self._headers(), timeout or self._timeout)

    async def connect_pty(
        self, id: str, *, cursor: int = 0, timeout: float | None = None
    ) -> AsyncPtyConnection:
        return await AsyncPtyConnection.connect(
            self.base_url.replace("http", "ws", 1)
            + "/v1/ptys/"
            + self.segment(id)
            + "/connect?cursor="
            + str(cursor),
            self._headers(),
            timeout or self._timeout,
        )


MIMES = {
    ".pdf": "application/pdf",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".webp": "image/webp",
    ".gif": "image/gif",
    # The server converts these to PNG or JPEG.
    ".heic": "image/heic",
    ".heif": "image/heif",
    ".bmp": "image/bmp",
    ".tif": "image/tiff",
    ".tiff": "image/tiff",
    ".mp3": "audio/mpeg",
    ".wav": "audio/wav",
    ".mp4": "video/mp4",
    ".webm": "video/webm",
    ".m4a": "audio/mp4",
    ".ogg": "audio/ogg",
    ".flac": "audio/flac",
    ".mov": "video/quicktime",
}


def attachments(paths: list[str | Path]) -> list[str]:
    """Encode client-local media for remote use, with a 20 MiB aggregate limit."""
    output = []
    size = 0
    for source in paths:
        path = Path(source)
        mime = MIMES.get(path.suffix.lower())
        if mime is None:
            raise ValueError("Unsupported attachment extension")
        with path.open("rb") as file:
            data = file.read(20 * 1024 * 1024 - size + 1)
        size += len(data)
        if size > 20 * 1024 * 1024:
            raise ValueError("Attachments exceed 20 MiB")
        output.append(
            f"data:{mime};name={quote(path.name, safe='')};base64,{base64.b64encode(data).decode()}"
        )
    return output
