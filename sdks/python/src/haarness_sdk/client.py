"""Convenience clients over the complete generated API."""

from __future__ import annotations
import base64
from pathlib import Path
from urllib.parse import quote
from uuid import uuid4
from typing_extensions import TypedDict
from .api import Api, AsyncApi
from .types import StartTurnRequest, Turn, Item, ChatOptions
from .transport import RequestOptions
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
    def workspace(self, id: str) -> Haarness:
        """Bind the complete runtime API to an opaque host workspace ID."""
        client = Haarness(self.base_url + '/v1/host/workspaces/' + self.segment(id), token='')
        client._authentication = self._headers()
        return client

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
    def workspace(self, id: str) -> AsyncHaarness:
        client = AsyncHaarness(self.base_url + '/v1/host/workspaces/' + self.segment(id), token='')
        client._authentication = self._headers()
        return client

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
