"""Cloud account API and explicit workspace and chat handles."""

from __future__ import annotations
import asyncio
import threading
from datetime import datetime, timezone
from urllib.parse import urlsplit
from .cloud_api import CloudApi, AsyncCloudApi
from .client import Haarness, AsyncHaarness
from .transport import ApiError, RequestOptions
from . import types as T, cloud_types as C
from .client import ChatResult
from .router import HaarnessRouter, AsyncHaarnessRouter
from .json_socket import JsonSocket, AsyncJsonSocket


def gateway(value: str) -> str:
    url = urlsplit(value)
    if (
        (
            url.scheme != "https"
            and not (
                url.scheme == "http"
                and url.hostname in ("localhost", "127.0.0.1", "::1")
            )
        )
        or url.username
        or url.password
        or url.query
        or url.fragment
    ):
        raise ValueError(
            "Cloud gateway requires HTTPS or loopback HTTP without credentials, query or fragment"
        )
    return value.rstrip("/")


def current(ticket: C.AttachResponse | None) -> bool:
    return bool(
        ticket
        and datetime.fromisoformat(
            ticket["expiresAt"].replace("Z", "+00:00")
        ).timestamp()
        > datetime.now(timezone.utc).timestamp() + 30
    )


class HaarnessCloud(CloudApi):
    def router(self, id: str, *, api_key: str) -> HaarnessRouter:
        """Select a router; supply an explicit inference-capable API key."""
        return HaarnessRouter(gateway(self.get_router(id)["routerUrl"]),token=api_key,timeout=self._timeout)

    def connect_remote_host(self, id: str, *, timeout: float | None = None) -> JsonSocket:
        """Host-side protocol; use the dedicated connector token as this client's token."""
        _, options = self.cloud_options(None, None, None)
        socket = JsonSocket(self.base_url.replace("http", "ws", 1) + "/api/v1/remote-hosts/" + self.segment(id) + "/connect", {**self._headers(), **options["headers"]}, timeout or self._timeout)
        self._sockets.add(socket)
        socket._on_close = lambda: self._sockets.discard(socket)
        return socket

    def remote_host(self, id: str) -> Haarness:
        """Full host gateway. Use workspace(project_id) for chats, files and PTYs."""
        return self._remote_runtime("/api/v1/remote-hosts/" + self.segment(id) + "/runtime")

    def remote_session(self, id: str) -> Haarness:
        """A published conversation tree and its shared workspace files."""
        return self._remote_runtime("/api/v1/remote-sessions/" + self.segment(id) + "/runtime")

    def _remote_runtime(self, path: str) -> Haarness:
        cloud = self
        class RemoteRuntime(Haarness):
            def _headers(self):
                _, options = cloud.cloud_options(None, None, None)
                return {**cloud._headers(), **options["headers"]}
        return RemoteRuntime(self.base_url + path, token="", timeout=self._timeout)

    def workspace(self, workspace_id: str) -> CloudWorkspace:
        """An existing Cloud workspace: its machine, files and chats."""
        self.segment(workspace_id)
        return CloudWorkspace(self, workspace_id)

    def new_workspace(
        self,
        body: C.CreateWorkspaceRequest | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> CloudWorkspace:
        """Create a workspace and return its handle; `create_workspace` answers the raw `{workspace, operation}`."""
        created = self.create_workspace(body or {}, options=options)
        workspace = self.workspace(created["workspace"]["id"])
        workspace.created = created
        return workspace


class CloudWorkspace:
    """One machine and shared filesystem; connect never starts compute."""

    def __init__(self, cloud: HaarnessCloud, workspace_id: str):
        self.cloud, self.id = cloud, workspace_id
        self.chats = CloudChats(self)
        # What `new_workspace` answered, with the first message's launch (`created["workspace"]["launch"]["chatId"]`).
        self.created: C.WorkspaceOperationResponse | None = None
        self._ticket: C.AttachResponse | None = None
        self._runtime: WorkspaceRuntime | None = None
        self._lock = threading.Lock()
        self._runtime_lock = threading.Lock()

    def get(self, *, options: RequestOptions | None = None) -> C.WorkspaceDto:
        return self.cloud.get_workspace(self.id, options=options)

    def start(
        self, *, options: RequestOptions | None = None
    ) -> C.WorkspaceOperationResponse:
        return self.cloud.start_workspace(self.id, options=options)

    def stop(
        self, *, options: RequestOptions | None = None
    ) -> C.WorkspaceOperationResponse:
        return self.cloud.stop_workspace(self.id, options=options)

    def delete(
        self, *, options: RequestOptions | None = None
    ) -> C.WorkspaceOperationResponse:
        return self.cloud.delete_workspace(self.id, options=options)

    def credentials(self) -> C.AttachResponse:
        with self._lock:
            if not current(self._ticket):
                ticket = self.cloud.attach_workspace(self.id)
                gateway(ticket["gatewayUrl"])
                self._ticket = ticket
            assert self._ticket is not None
            return self._ticket

    def connect(self) -> WorkspaceRuntime:
        with self._runtime_lock:
            if self._runtime is None:
                ticket = self.credentials()
                self._runtime = WorkspaceRuntime(self, ticket)
            return self._runtime

    def close(self) -> None:
        with self._runtime_lock:
            if self._runtime is not None:
                self._runtime.close()
            self._runtime = None
            self._ticket = None


class WorkspaceRuntime(Haarness):
    def __init__(self, workspace: CloudWorkspace, ticket):
        super().__init__(
            gateway(ticket["gatewayUrl"]),
            token=ticket["ticket"],
            timeout=workspace.cloud._timeout,
        )
        self.cloud_workspace = workspace

    def refresh(self):
        ticket = self.cloud_workspace.credentials()
        self.base_url = gateway(ticket["gatewayUrl"])
        self._authentication = {"Authorization": "Bearer " + ticket["ticket"]}

    def request(self, method, path, **kwargs):
        for attempt in range(2):
            self.refresh()
            try:
                return super().request(method, path, **kwargs)
            except ApiError as error:
                stale = (
                    method == "GET"
                    and error.status == 400
                    and error.code == "remote_project"
                    and "workspace restarted" in str(error)
                )
                if attempt or not (error.status == 401 or stale):
                    raise
                self.cloud_workspace._ticket = None

    def _prepare_events(self) -> None:
        self.refresh()

    def _retry_events(self, error: ApiError) -> bool:
        if error.status == 401 or (
            error.status == 400
            and error.code == "remote_project"
            and "workspace restarted" in str(error)
        ):
            self.cloud_workspace._ticket = None
            return True
        return False

    def connect_pty(self, id, **kwargs):
        self.refresh()
        return super().connect_pty(id, **kwargs)


class CloudChats:
    def __init__(self, workspace: CloudWorkspace):
        self.workspace = workspace

    def list(
        self,
        query: T.ThreadQuery | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> list[T.Thread]:
        runtime = self.workspace.connect()
        return [
            t
            for t in runtime.list_threads(query, options=options)
            if not t.get("parentId")
        ]

    def create(
        self,
        body: T.CreateThreadRequest | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> CloudChat:
        runtime = self.workspace.connect()
        info = runtime.info(options=options)
        thread = runtime.create_thread(
            {
                "agent": info.get("defaultAgent"),
                "provider": info.get("defaultProvider"),
                "model": info.get("defaultModel"),
                **(body or {}),
            },
            options=options,
        )
        return self.get(thread["id"])

    def get(self, chat_id: str) -> CloudChat:
        Haarness.segment(chat_id)
        return CloudChat(self.workspace, chat_id)


class CloudChat:
    """Chat ID equals runtime thread ID. History is separate; files are shared."""

    def __init__(self, workspace: CloudWorkspace, chat_id: str):
        self.workspace, self.id = workspace, chat_id

    @property
    def workspace_id(self) -> str:
        return self.workspace.id

    def get(self, *, options: RequestOptions | None = None) -> T.Thread:
        runtime = self.workspace.connect()
        return runtime.get_thread(self.id, options=options)

    def rename(self, title: str, *, options: RequestOptions | None = None) -> T.Thread:
        runtime = self.workspace.connect()
        return runtime.update_thread(self.id, {"title": title}, options=options)

    def archive(
        self, archived: bool = True, *, options: RequestOptions | None = None
    ) -> T.Thread:
        runtime = self.workspace.connect()
        return runtime.update_thread(self.id, {"archived": archived}, options=options)

    def messages(
        self, after: int | None = None, *, options: RequestOptions | None = None
    ) -> list[T.Item]:
        runtime = self.workspace.connect()
        return runtime.list_items(
            self.id, {} if after is None else {"after": after}, options=options
        )

    def ask(
        self,
        text: str,
        body: T.ChatOptions | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> ChatResult:
        runtime = self.workspace.connect()
        return runtime.chat(self.id, text, body, options=options)

    def send(
        self, body: T.StartTurnRequest, *, options: RequestOptions | None = None
    ) -> T.Turn:
        runtime = self.workspace.connect()
        return runtime.start_turn(self.id, body, options=options)

    def _turn(self, turn_id: str, options: RequestOptions | None) -> WorkspaceRuntime:
        runtime = self.workspace.connect()
        turn = runtime.get_turn(turn_id, options=options)
        if turn["threadId"] != self.id:
            raise ValueError("Turn belongs to another chat")
        return runtime

    def wait(
        self,
        turn_id: str,
        timeout_ms: int = 120000,
        *,
        options: RequestOptions | None = None,
    ) -> T.Turn:
        runtime = self._turn(turn_id, options)
        return runtime.wait_turn(
            turn_id,
            {"timeout_ms": timeout_ms},
            options={"timeout": timeout_ms / 1000 + 5, **(options or {})},
        )

    def abort(
        self, turn_id: str | None = None, *, options: RequestOptions | None = None
    ) -> T.Turn | T.InterruptResponse:
        if turn_id:
            runtime = self._turn(turn_id, options)
            return runtime.interrupt_turn(turn_id, options=options)
        runtime = self.workspace.connect()
        return runtime.interrupt_thread_tree(self.id, options=options)


class AsyncHaarnessCloud(AsyncCloudApi):
    async def router(self, id: str, *, api_key: str) -> AsyncHaarnessRouter:
        return AsyncHaarnessRouter(gateway((await self.get_router(id))["routerUrl"]),token=api_key,timeout=self._timeout)

    async def connect_remote_host(self, id: str, *, timeout: float | None = None) -> AsyncJsonSocket:
        _, options = self.cloud_options(None, None, None)
        socket = await AsyncJsonSocket.connect(self.base_url.replace("http", "ws", 1) + "/api/v1/remote-hosts/" + self.segment(id) + "/connect", {**self._headers(), **options["headers"]}, timeout or self._timeout)
        self._sockets.add(socket)
        socket._on_close = lambda: self._sockets.discard(socket)
        return socket

    def remote_host(self, id: str) -> AsyncHaarness:
        """Full host gateway. Use workspace(project_id) for chats, files and PTYs."""
        return self._remote_runtime("/api/v1/remote-hosts/" + self.segment(id) + "/runtime")

    def remote_session(self, id: str) -> AsyncHaarness:
        """A published conversation tree and its shared workspace files."""
        return self._remote_runtime("/api/v1/remote-sessions/" + self.segment(id) + "/runtime")

    def _remote_runtime(self, path: str) -> AsyncHaarness:
        cloud = self
        class RemoteRuntime(AsyncHaarness):
            def _headers(self):
                _, options = cloud.cloud_options(None, None, None)
                return {**cloud._headers(), **options["headers"]}
        return RemoteRuntime(self.base_url + path, token="", timeout=self._timeout)

    def workspace(self, workspace_id: str) -> AsyncCloudWorkspace:
        """An existing Cloud workspace: its machine, files and chats."""
        self.segment(workspace_id)
        return AsyncCloudWorkspace(self, workspace_id)

    async def new_workspace(
        self,
        body: C.CreateWorkspaceRequest | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> AsyncCloudWorkspace:
        """Create a workspace and return its handle; `create_workspace` answers the raw `{workspace, operation}`."""
        created = await self.create_workspace(body or {}, options=options)
        workspace = self.workspace(created["workspace"]["id"])
        workspace.created = created
        return workspace


class AsyncCloudWorkspace:
    """One machine and shared filesystem; connect never starts compute."""

    def __init__(self, cloud: AsyncHaarnessCloud, workspace_id: str):
        self.cloud, self.id = cloud, workspace_id
        self.chats = AsyncCloudChats(self)
        # What `new_workspace` answered, with the first message's launch (`created["workspace"]["launch"]["chatId"]`).
        self.created: C.WorkspaceOperationResponse | None = None
        self._ticket: C.AttachResponse | None = None
        self._runtime: AsyncWorkspaceRuntime | None = None
        self._lock = asyncio.Lock()
        self._runtime_lock = asyncio.Lock()

    async def get(self, *, options: RequestOptions | None = None) -> C.WorkspaceDto:
        return await self.cloud.get_workspace(self.id, options=options)

    async def start(
        self, *, options: RequestOptions | None = None
    ) -> C.WorkspaceOperationResponse:
        return await self.cloud.start_workspace(self.id, options=options)

    async def stop(
        self, *, options: RequestOptions | None = None
    ) -> C.WorkspaceOperationResponse:
        return await self.cloud.stop_workspace(self.id, options=options)

    async def delete(
        self, *, options: RequestOptions | None = None
    ) -> C.WorkspaceOperationResponse:
        return await self.cloud.delete_workspace(self.id, options=options)

    async def credentials(self) -> C.AttachResponse:
        async with self._lock:
            if not current(self._ticket):
                ticket = await self.cloud.attach_workspace(self.id)
                gateway(ticket["gatewayUrl"])
                self._ticket = ticket
            assert self._ticket is not None
            return self._ticket

    async def connect(self) -> AsyncWorkspaceRuntime:
        async with self._runtime_lock:
            if self._runtime is None:
                ticket = await self.credentials()
                self._runtime = AsyncWorkspaceRuntime(self, ticket)
            return self._runtime

    async def close(self) -> None:
        async with self._runtime_lock:
            if self._runtime is not None:
                await self._runtime.close()
            self._runtime = None
            self._ticket = None


class AsyncWorkspaceRuntime(AsyncHaarness):
    def __init__(self, workspace: AsyncCloudWorkspace, ticket):
        super().__init__(
            gateway(ticket["gatewayUrl"]),
            token=ticket["ticket"],
            timeout=workspace.cloud._timeout,
        )
        self.cloud_workspace = workspace

    async def refresh(self):
        ticket = await self.cloud_workspace.credentials()
        self.base_url = gateway(ticket["gatewayUrl"])
        self._authentication = {"Authorization": "Bearer " + ticket["ticket"]}

    async def request(self, method, path, **kwargs):
        for attempt in range(2):
            await self.refresh()
            try:
                return await super().request(method, path, **kwargs)
            except ApiError as error:
                stale = (
                    method == "GET"
                    and error.status == 400
                    and error.code == "remote_project"
                    and "workspace restarted" in str(error)
                )
                if attempt or not (error.status == 401 or stale):
                    raise
                self.cloud_workspace._ticket = None

    async def _prepare_events(self) -> None:
        await self.refresh()

    async def _retry_events(self, error: ApiError) -> bool:
        if error.status == 401 or (
            error.status == 400
            and error.code == "remote_project"
            and "workspace restarted" in str(error)
        ):
            self.cloud_workspace._ticket = None
            return True
        return False

    async def connect_pty(self, id, **kwargs):
        await self.refresh()
        return await super().connect_pty(id, **kwargs)


class AsyncCloudChats:
    def __init__(self, workspace: AsyncCloudWorkspace):
        self.workspace = workspace

    async def list(
        self,
        query: T.ThreadQuery | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> list[T.Thread]:
        runtime = await self.workspace.connect()
        return [
            t
            for t in (await runtime.list_threads(query, options=options))
            if not t.get("parentId")
        ]

    async def create(
        self,
        body: T.CreateThreadRequest | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> AsyncCloudChat:
        runtime = await self.workspace.connect()
        info = await runtime.info(options=options)
        thread = await runtime.create_thread(
            {
                "agent": info.get("defaultAgent"),
                "provider": info.get("defaultProvider"),
                "model": info.get("defaultModel"),
                **(body or {}),
            },
            options=options,
        )
        return self.get(thread["id"])

    def get(self, chat_id: str) -> AsyncCloudChat:
        AsyncHaarness.segment(chat_id)
        return AsyncCloudChat(self.workspace, chat_id)


class AsyncCloudChat:
    """Chat ID equals runtime thread ID. History is separate; files are shared."""

    def __init__(self, workspace: AsyncCloudWorkspace, chat_id: str):
        self.workspace, self.id = workspace, chat_id

    @property
    def workspace_id(self) -> str:
        return self.workspace.id

    async def get(self, *, options: RequestOptions | None = None) -> T.Thread:
        runtime = await self.workspace.connect()
        return await runtime.get_thread(self.id, options=options)

    async def rename(
        self, title: str, *, options: RequestOptions | None = None
    ) -> T.Thread:
        runtime = await self.workspace.connect()
        return await runtime.update_thread(self.id, {"title": title}, options=options)

    async def archive(
        self, archived: bool = True, *, options: RequestOptions | None = None
    ) -> T.Thread:
        runtime = await self.workspace.connect()
        return await runtime.update_thread(
            self.id, {"archived": archived}, options=options
        )

    async def messages(
        self, after: int | None = None, *, options: RequestOptions | None = None
    ) -> list[T.Item]:
        runtime = await self.workspace.connect()
        return await runtime.list_items(
            self.id, {} if after is None else {"after": after}, options=options
        )

    async def ask(
        self,
        text: str,
        body: T.ChatOptions | None = None,
        *,
        options: RequestOptions | None = None,
    ) -> ChatResult:
        runtime = await self.workspace.connect()
        return await runtime.chat(self.id, text, body, options=options)

    async def send(
        self, body: T.StartTurnRequest, *, options: RequestOptions | None = None
    ) -> T.Turn:
        runtime = await self.workspace.connect()
        return await runtime.start_turn(self.id, body, options=options)

    async def _turn(
        self, turn_id: str, options: RequestOptions | None
    ) -> AsyncWorkspaceRuntime:
        runtime = await self.workspace.connect()
        turn = await runtime.get_turn(turn_id, options=options)
        if turn["threadId"] != self.id:
            raise ValueError("Turn belongs to another chat")
        return runtime

    async def wait(
        self,
        turn_id: str,
        timeout_ms: int = 120000,
        *,
        options: RequestOptions | None = None,
    ) -> T.Turn:
        runtime = await self._turn(turn_id, options)
        return await runtime.wait_turn(
            turn_id,
            {"timeout_ms": timeout_ms},
            options={"timeout": timeout_ms / 1000 + 5, **(options or {})},
        )

    async def abort(
        self, turn_id: str | None = None, *, options: RequestOptions | None = None
    ) -> T.Turn | T.InterruptResponse:
        if turn_id:
            runtime = await self._turn(turn_id, options)
            return await runtime.interrupt_turn(turn_id, options=options)
        runtime = await self.workspace.connect()
        return await runtime.interrupt_thread_tree(self.id, options=options)
