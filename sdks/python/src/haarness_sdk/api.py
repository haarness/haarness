"""Generated endpoint methods. Do not edit."""
from typing import cast, Union
from . import types as T
from .transport import Transport, AsyncTransport, RequestOptions
class Api(Transport):
    def manage_resource(self, resource: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/manage/{resource}."""
        return cast("T.JsonValue", self.request('POST', f'/v1/manage/{self.segment(resource)}', body=body, binary_body=False, binary_response=False, options=options))

    def cloud_agents(self, body: T.CloudAgentInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/agents."""
        return cast("T.JsonValue", self.request('POST', f'/v1/cloud/agents', body=body, binary_body=False, binary_response=False, options=options))

    def cloud_workspaces(self, body: T.CloudWorkspaceInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/workspaces."""
        return cast("T.JsonValue", self.request('POST', f'/v1/cloud/workspaces', body=body, binary_body=False, binary_response=False, options=options))

    def cloud_chats(self, body: T.CloudChatInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/chats."""
        return cast("T.JsonValue", self.request('POST', f'/v1/cloud/chats', body=body, binary_body=False, binary_response=False, options=options))

    def cloud_schedules(self, body: T.CloudScheduleInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/schedules."""
        return cast("T.JsonValue", self.request('POST', f'/v1/cloud/schedules', body=body, binary_body=False, binary_response=False, options=options))

    def info(self, *, options: RequestOptions | None = None) -> T.ServerInfo:
        """GET /v1/info."""
        return cast("T.ServerInfo", self.request('GET', f'/v1/info', binary_body=False, binary_response=False, options=options))

    def analytics_stats(self, query: T.AnalyticsQuery | None = None, *, options: RequestOptions | None = None) -> T.Report:
        """GET /v1/stats."""
        return cast("T.Report", self.request('GET', f'/v1/stats', query=query, binary_body=False, binary_response=False, options=options))

    def analytics_insights(self, query: T.AnalyticsQuery | None = None, *, options: RequestOptions | None = None) -> T.AnalyticsDocument:
        """GET /v1/insights."""
        return cast("T.AnalyticsDocument", self.request('GET', f'/v1/insights', query=query, binary_body=False, binary_response=False, options=options))

    def providers(self, *, options: RequestOptions | None = None) -> list[T.ProviderInfo]:
        """GET /v1/providers."""
        return cast("list[T.ProviderInfo]", self.request('GET', f'/v1/providers', binary_body=False, binary_response=False, options=options))

    def refresh_providers(self, *, options: RequestOptions | None = None) -> list[T.ProviderInfo]:
        """POST /v1/providers/refresh."""
        return cast("list[T.ProviderInfo]", self.request('POST', f'/v1/providers/refresh', binary_body=False, binary_response=False, options=options))

    def agents(self, *, options: RequestOptions | None = None) -> list[T.AgentProfile]:
        """GET /v1/agents."""
        return cast("list[T.AgentProfile]", self.request('GET', f'/v1/agents', binary_body=False, binary_response=False, options=options))

    def commands(self, query: T.ChatListQuery | None = None, *, options: RequestOptions | None = None) -> list[T.CommandInfo]:
        """GET /v1/commands."""
        return cast("list[T.CommandInfo]", self.request('GET', f'/v1/commands', query=query, binary_body=False, binary_response=False, options=options))

    def lsp_status(self, *, options: RequestOptions | None = None) -> list[T.LanguageStatus]:
        """GET /v1/lsp."""
        return cast("list[T.LanguageStatus]", self.request('GET', f'/v1/lsp', binary_body=False, binary_response=False, options=options))

    def formatter_status(self, *, options: RequestOptions | None = None) -> list[T.FormatterStatus]:
        """GET /v1/formatter."""
        return cast("list[T.FormatterStatus]", self.request('GET', f'/v1/formatter', binary_body=False, binary_response=False, options=options))

    def tool_catalog(self, query: T.ToolCatalogQuery | None = None, *, options: RequestOptions | None = None) -> T.ToolCatalogResponse:
        """GET /v1/tools."""
        return cast("T.ToolCatalogResponse", self.request('GET', f'/v1/tools', query=query, binary_body=False, binary_response=False, options=options))

    def update_lazy_tools(self, body: T.LazyToolSettingsUpdate, *, options: RequestOptions | None = None) -> T.ToolCatalogResponse:
        """POST /v1/tools/lazy."""
        return cast("T.ToolCatalogResponse", self.request('POST', f'/v1/tools/lazy', body=body, binary_body=False, binary_response=False, options=options))

    def update_tool(self, name: str, body: T.ToolSettingsUpdate, *, options: RequestOptions | None = None) -> T.ToolCatalogResponse:
        """POST /v1/tools/{name}."""
        return cast("T.ToolCatalogResponse", self.request('POST', f'/v1/tools/{self.segment(name)}', body=body, binary_body=False, binary_response=False, options=options))

    def projects(self, *, options: RequestOptions | None = None) -> list[T.ProjectInfo]:
        """GET /v1/projects."""
        return cast("list[T.ProjectInfo]", self.request('GET', f'/v1/projects', binary_body=False, binary_response=False, options=options))

    def workspaces_list(self, *, options: RequestOptions | None = None) -> T.WorkspaceListResponse:
        """GET /v1/workspaces."""
        return cast("T.WorkspaceListResponse", self.request('GET', f'/v1/workspaces', binary_body=False, binary_response=False, options=options))

    def create_workspace(self, body: T.WorkspaceCreateRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/v1/workspaces', body=body, binary_body=False, binary_response=False, options=options))

    def workspace_refs(self, *, options: RequestOptions | None = None) -> T.WorkspaceRefsResponse:
        """GET /v1/workspaces/refs."""
        return cast("T.WorkspaceRefsResponse", self.request('GET', f'/v1/workspaces/refs', binary_body=False, binary_response=False, options=options))

    def resolve_workspace_ref(self, body: T.WorkspaceResolveRefRequest, *, options: RequestOptions | None = None) -> T.WorkspaceResolveRefResponse:
        """POST /v1/workspaces/resolve-ref."""
        return cast("T.WorkspaceResolveRefResponse", self.request('POST', f'/v1/workspaces/resolve-ref', body=body, binary_body=False, binary_response=False, options=options))

    def get_workspace(self, reference: str, *, options: RequestOptions | None = None) -> T.WorkspaceInfo:
        """GET /v1/workspaces/{reference}."""
        return cast("T.WorkspaceInfo", self.request('GET', f'/v1/workspaces/{self.segment(reference)}', binary_body=False, binary_response=False, options=options))

    def remove_workspace(self, reference: str, body: T.WorkspaceRemoveRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """DELETE /v1/workspaces/{reference}."""
        return cast("T.WorkspaceOperationResponse", self.request('DELETE', f'/v1/workspaces/{self.segment(reference)}', body=body, binary_body=False, binary_response=False, options=options))

    def open_workspace(self, reference: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces/{reference}/open."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/v1/workspaces/{self.segment(reference)}/open', binary_body=False, binary_response=False, options=options))

    def reset_workspace(self, reference: str, body: T.WorkspaceResetRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces/{reference}/reset."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/v1/workspaces/{self.segment(reference)}/reset', body=body, binary_body=False, binary_response=False, options=options))

    def cleanup_workspace(self, reference: str, body: T.WorkspaceCleanupRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces/{reference}/cleanup."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/v1/workspaces/{self.segment(reference)}/cleanup', body=body, binary_body=False, binary_response=False, options=options))

    def list_ptys(self, *, options: RequestOptions | None = None) -> list[T.PtyInfo]:
        """GET /v1/ptys."""
        return cast("list[T.PtyInfo]", self.request('GET', f'/v1/ptys', binary_body=False, binary_response=False, options=options))

    def create_pty(self, body: T.PtyCreateRequest, *, options: RequestOptions | None = None) -> T.PtyInfo:
        """POST /v1/ptys."""
        return cast("T.PtyInfo", self.request('POST', f'/v1/ptys', body=body, binary_body=False, binary_response=False, options=options))

    def get_pty(self, id: str, *, options: RequestOptions | None = None) -> T.PtyInfo:
        """GET /v1/ptys/{id}."""
        return cast("T.PtyInfo", self.request('GET', f'/v1/ptys/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_pty(self, id: str, body: T.PtyUpdateRequest, *, options: RequestOptions | None = None) -> T.PtyInfo:
        """PATCH /v1/ptys/{id}."""
        return cast("T.PtyInfo", self.request('PATCH', f'/v1/ptys/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    def delete_pty(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/ptys/{id}."""
        return cast("None", self.request('DELETE', f'/v1/ptys/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def workflows(self, query: T.ChatListQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkflowListResponse:
        """GET /v1/workflows."""
        return cast("T.WorkflowListResponse", self.request('GET', f'/v1/workflows', query=query, binary_body=False, binary_response=False, options=options))

    def workflow_source(self, invocation: str, *, options: RequestOptions | None = None) -> T.WorkflowSourceResponse:
        """GET /v1/workflows/{invocation}/source."""
        return cast("T.WorkflowSourceResponse", self.request('GET', f'/v1/workflows/{self.segment(invocation)}/source', binary_body=False, binary_response=False, options=options))

    def start_workflow(self, body: T.WorkflowStartRequest, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/run."""
        return cast("T.WorkflowRun", self.request('POST', f'/v1/workflows/run', body=body, binary_body=False, binary_response=False, options=options))

    def list_workflow_runs(self, query: T.WorkflowRunsQuery | None = None, *, options: RequestOptions | None = None) -> list[T.WorkflowRun]:
        """GET /v1/workflows/runs."""
        return cast("list[T.WorkflowRun]", self.request('GET', f'/v1/workflows/runs', query=query, binary_body=False, binary_response=False, options=options))

    def get_workflow_run(self, id: str, query: T.WorkflowRunQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """GET /v1/workflows/runs/{id}."""
        return cast("T.WorkflowRun", self.request('GET', f'/v1/workflows/runs/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    def delete_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/workflows/runs/{id}."""
        return cast("None", self.request('DELETE', f'/v1/workflows/runs/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def wait_workflow_run(self, id: str, query: T.WorkflowWaitQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """GET /v1/workflows/runs/{id}/wait."""
        return cast("T.WorkflowRun", self.request('GET', f'/v1/workflows/runs/{self.segment(id)}/wait', query=query, binary_body=False, binary_response=False, options=options))

    def cancel_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/runs/{id}/cancel."""
        return cast("T.WorkflowRun", self.request('POST', f'/v1/workflows/runs/{self.segment(id)}/cancel', binary_body=False, binary_response=False, options=options))

    def pause_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/runs/{id}/pause."""
        return cast("T.WorkflowRun", self.request('POST', f'/v1/workflows/runs/{self.segment(id)}/pause', binary_body=False, binary_response=False, options=options))

    def resume_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/runs/{id}/resume."""
        return cast("T.WorkflowRun", self.request('POST', f'/v1/workflows/runs/{self.segment(id)}/resume', binary_body=False, binary_response=False, options=options))

    def skills(self, *, options: RequestOptions | None = None) -> list[T.SkillInfo]:
        """GET /v1/skills."""
        return cast("list[T.SkillInfo]", self.request('GET', f'/v1/skills', binary_body=False, binary_response=False, options=options))

    def load_skill(self, name: str, *, options: RequestOptions | None = None) -> T.SkillContent:
        """GET /v1/skills/{name}."""
        return cast("T.SkillContent", self.request('GET', f'/v1/skills/{self.segment(name)}', binary_body=False, binary_response=False, options=options))

    def plugins(self, *, options: RequestOptions | None = None) -> list[T.PluginInfo]:
        """GET /v1/plugins."""
        return cast("list[T.PluginInfo]", self.request('GET', f'/v1/plugins', binary_body=False, binary_response=False, options=options))

    def plugin_action(self, body: T.PluginActionRequest, *, options: RequestOptions | None = None) -> list[T.PluginInfo]:
        """POST /v1/plugins."""
        return cast("list[T.PluginInfo]", self.request('POST', f'/v1/plugins', body=body, binary_body=False, binary_response=False, options=options))

    def mcp_servers(self, query: T.McpServersQuery | None = None, *, options: RequestOptions | None = None) -> list[T.McpServerInfo]:
        """GET /v1/mcp."""
        return cast("list[T.McpServerInfo]", self.request('GET', f'/v1/mcp', query=query, binary_body=False, binary_response=False, options=options))

    def configure_mcp(self, name: str, body: T.McpConfigureRequest, *, options: RequestOptions | None = None) -> T.McpServerInfo:
        """POST /v1/mcp/{name}."""
        return cast("T.McpServerInfo", self.request('POST', f'/v1/mcp/{self.segment(name)}', body=body, binary_body=False, binary_response=False, options=options))

    def reconnect_mcp(self, name: str, *, options: RequestOptions | None = None) -> T.McpServerInfo:
        """POST /v1/mcp/{name}/reconnect."""
        return cast("T.McpServerInfo", self.request('POST', f'/v1/mcp/{self.segment(name)}/reconnect', binary_body=False, binary_response=False, options=options))

    def disconnect_mcp(self, name: str, *, options: RequestOptions | None = None) -> T.McpServerInfo:
        """POST /v1/mcp/{name}/disconnect."""
        return cast("T.McpServerInfo", self.request('POST', f'/v1/mcp/{self.segment(name)}/disconnect', binary_body=False, binary_response=False, options=options))

    def mcp_tools(self, name: str, *, options: RequestOptions | None = None) -> list[T.ToolDefinition]:
        """GET /v1/mcp/{name}/tools."""
        return cast("list[T.ToolDefinition]", self.request('GET', f'/v1/mcp/{self.segment(name)}/tools', binary_body=False, binary_response=False, options=options))

    def call_mcp_tool(self, name: str, tool: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.ToolOutput:
        """POST /v1/mcp/{name}/tools/{tool}."""
        return cast("T.ToolOutput", self.request('POST', f'/v1/mcp/{self.segment(name)}/tools/{self.segment(tool)}', body=body, binary_body=False, binary_response=False, options=options))

    def mcp_resources(self, name: str, *, options: RequestOptions | None = None) -> list[T.JsonValue]:
        """GET /v1/mcp/{name}/resources."""
        return cast("list[T.JsonValue]", self.request('GET', f'/v1/mcp/{self.segment(name)}/resources', binary_body=False, binary_response=False, options=options))

    def mcp_resource_templates(self, name: str, *, options: RequestOptions | None = None) -> list[T.JsonValue]:
        """GET /v1/mcp/{name}/resource-templates."""
        return cast("list[T.JsonValue]", self.request('GET', f'/v1/mcp/{self.segment(name)}/resource-templates', binary_body=False, binary_response=False, options=options))

    def read_mcp_resource(self, name: str, body: T.ResourceReadRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/mcp/{name}/resources/read."""
        return cast("T.JsonValue", self.request('POST', f'/v1/mcp/{self.segment(name)}/resources/read', body=body, binary_body=False, binary_response=False, options=options))

    def mcp_prompts(self, name: str, *, options: RequestOptions | None = None) -> list[T.JsonValue]:
        """GET /v1/mcp/{name}/prompts."""
        return cast("list[T.JsonValue]", self.request('GET', f'/v1/mcp/{self.segment(name)}/prompts', binary_body=False, binary_response=False, options=options))

    def get_mcp_prompt(self, name: str, prompt: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/mcp/{name}/prompts/{prompt}."""
        return cast("T.JsonValue", self.request('POST', f'/v1/mcp/{self.segment(name)}/prompts/{self.segment(prompt)}', body=body, binary_body=False, binary_response=False, options=options))

    def list_threads(self, query: T.ThreadQuery | None = None, *, options: RequestOptions | None = None) -> list[T.Thread]:
        """GET /v1/threads."""
        return cast("list[T.Thread]", self.request('GET', f'/v1/threads', query=query, binary_body=False, binary_response=False, options=options))

    def create_thread(self, body: T.CreateThreadRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads."""
        return cast("T.Thread", self.request('POST', f'/v1/threads', body=body, binary_body=False, binary_response=False, options=options))

    def get_thread(self, id: str, *, options: RequestOptions | None = None) -> T.Thread:
        """GET /v1/threads/{id}."""
        return cast("T.Thread", self.request('GET', f'/v1/threads/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_thread(self, id: str, body: T.UpdateThreadRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """PATCH /v1/threads/{id}."""
        return cast("T.Thread", self.request('PATCH', f'/v1/threads/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    def export_session_archive(self, id: str, query: T.SessionArchiveQuery | None = None, *, options: RequestOptions | None = None) -> T.SessionArchive:
        """GET /v1/threads/{id}/archive."""
        return cast("T.SessionArchive", self.request('GET', f'/v1/threads/{self.segment(id)}/archive', query=query, binary_body=False, binary_response=False, options=options))

    def fork_thread(self, id: str, body: T.ForkThreadRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads/{id}/fork."""
        return cast("T.Thread", self.request('POST', f'/v1/threads/{self.segment(id)}/fork', body=body, binary_body=False, binary_response=False, options=options))

    def list_items(self, id: str, query: T.ItemsQuery | None = None, *, options: RequestOptions | None = None) -> list[T.Item]:
        """GET /v1/threads/{id}/items."""
        return cast("list[T.Item]", self.request('GET', f'/v1/threads/{self.segment(id)}/items', query=query, binary_body=False, binary_response=False, options=options))

    def list_todos(self, id: str, *, options: RequestOptions | None = None) -> list[T.TodoEntry]:
        """GET /v1/threads/{id}/todos."""
        return cast("list[T.TodoEntry]", self.request('GET', f'/v1/threads/{self.segment(id)}/todos', binary_body=False, binary_response=False, options=options))

    def active_turn(self, id: str, *, options: RequestOptions | None = None) -> Union[T.Turn, None]:
        """GET /v1/threads/{id}/active."""
        return cast("Union[T.Turn, None]", self.request('GET', f'/v1/threads/{self.segment(id)}/active', binary_body=False, binary_response=False, options=options))

    def thread_activity(self, id: str, *, options: RequestOptions | None = None) -> T.ThreadActivity:
        """GET /v1/threads/{id}/activity."""
        return cast("T.ThreadActivity", self.request('GET', f'/v1/threads/{self.segment(id)}/activity', binary_body=False, binary_response=False, options=options))

    def interrupt_thread_tree(self, id: str, query: T.InterruptQuery | None = None, *, options: RequestOptions | None = None) -> T.InterruptResponse:
        """POST /v1/threads/{id}/interrupt."""
        return cast("T.InterruptResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/interrupt', query=query, binary_body=False, binary_response=False, options=options))

    def list_background_tasks(self, id: str, *, options: RequestOptions | None = None) -> list[T.BackgroundTaskInfo]:
        """GET /v1/threads/{id}/background."""
        return cast("list[T.BackgroundTaskInfo]", self.request('GET', f'/v1/threads/{self.segment(id)}/background', binary_body=False, binary_response=False, options=options))

    def promote_background_tasks(self, id: str, query: T.BackgroundQuery | None = None, *, options: RequestOptions | None = None) -> list[T.BackgroundTaskInfo]:
        """POST /v1/threads/{id}/background."""
        return cast("list[T.BackgroundTaskInfo]", self.request('POST', f'/v1/threads/{self.segment(id)}/background', query=query, binary_body=False, binary_response=False, options=options))

    def pending_question(self, id: str, *, options: RequestOptions | None = None) -> Union[T.PendingQuestionInfo, None]:
        """GET /v1/threads/{id}/pending-question."""
        return cast("Union[T.PendingQuestionInfo, None]", self.request('GET', f'/v1/threads/{self.segment(id)}/pending-question', binary_body=False, binary_response=False, options=options))

    def start_turn(self, id: str, body: T.StartTurnRequest, *, options: RequestOptions | None = None) -> T.Turn:
        """POST /v1/threads/{id}/turns."""
        return cast("T.Turn", self.request('POST', f'/v1/threads/{self.segment(id)}/turns', body=body, binary_body=False, binary_response=False, options=options))

    def execute_command(self, id: str, body: T.ExecuteCommandRequest, *, options: RequestOptions | None = None) -> T.CommandExecutionResponse:
        """POST /v1/threads/{id}/commands."""
        return cast("T.CommandExecutionResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/commands', body=body, binary_body=False, binary_response=False, options=options))

    def queue_input(self, id: str, body: T.QueueInputRequest, *, options: RequestOptions | None = None) -> T.QueueInputResponse:
        """POST /v1/threads/{id}/input."""
        return cast("T.QueueInputResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/input', body=body, binary_body=False, binary_response=False, options=options))

    def input_queue(self, id: str, *, options: RequestOptions | None = None) -> T.InputQueue:
        """GET /v1/threads/{id}/input/queue."""
        return cast("T.InputQueue", self.request('GET', f'/v1/threads/{self.segment(id)}/input/queue', binary_body=False, binary_response=False, options=options))

    def change_input_queue(self, id: str, body: T.InputQueueAction, *, options: RequestOptions | None = None) -> T.InputQueue:
        """POST /v1/threads/{id}/input/queue."""
        return cast("T.InputQueue", self.request('POST', f'/v1/threads/{self.segment(id)}/input/queue', body=body, binary_body=False, binary_response=False, options=options))

    def btw(self, id: str, body: T.BtwRequest, *, options: RequestOptions | None = None) -> T.BtwResponse:
        """POST /v1/threads/{id}/btw."""
        return cast("T.BtwResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/btw', body=body, binary_body=False, binary_response=False, options=options))

    def compact(self, id: str, body: T.CompactRequest, *, options: RequestOptions | None = None) -> T.Item:
        """POST /v1/threads/{id}/compact."""
        return cast("T.Item", self.request('POST', f'/v1/threads/{self.segment(id)}/compact', body=body, binary_body=False, binary_response=False, options=options))

    def get_goal(self, id: str, *, options: RequestOptions | None = None) -> T.GoalActionResponse:
        """GET /v1/threads/{id}/goal."""
        return cast("T.GoalActionResponse", self.request('GET', f'/v1/threads/{self.segment(id)}/goal', binary_body=False, binary_response=False, options=options))

    def set_goal(self, id: str, body: T.GoalRequest, *, options: RequestOptions | None = None) -> T.GoalActionResponse:
        """POST /v1/threads/{id}/goal."""
        return cast("T.GoalActionResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/goal', body=body, binary_body=False, binary_response=False, options=options))

    def get_approval_mode(self, id: str, *, options: RequestOptions | None = None) -> T.ApprovalModeInfo:
        """GET /v1/threads/{id}/approval-mode."""
        return cast("T.ApprovalModeInfo", self.request('GET', f'/v1/threads/{self.segment(id)}/approval-mode', binary_body=False, binary_response=False, options=options))

    def set_approval_mode(self, id: str, body: T.SetApprovalModeRequest, *, options: RequestOptions | None = None) -> T.ApprovalModeInfo:
        """PUT /v1/threads/{id}/approval-mode."""
        return cast("T.ApprovalModeInfo", self.request('PUT', f'/v1/threads/{self.segment(id)}/approval-mode', body=body, binary_body=False, binary_response=False, options=options))

    def get_loop(self, id: str, *, options: RequestOptions | None = None) -> T.LoopResponse:
        """GET /v1/threads/{id}/loop."""
        return cast("T.LoopResponse", self.request('GET', f'/v1/threads/{self.segment(id)}/loop', binary_body=False, binary_response=False, options=options))

    def set_loop(self, id: str, body: T.LoopRequest, *, options: RequestOptions | None = None) -> T.LoopResponse:
        """POST /v1/threads/{id}/loop."""
        return cast("T.LoopResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/loop', body=body, binary_body=False, binary_response=False, options=options))

    def thread_usage(self, id: str, *, options: RequestOptions | None = None) -> T.UsageResponse:
        """GET /v1/threads/{id}/usage."""
        return cast("T.UsageResponse", self.request('GET', f'/v1/threads/{self.segment(id)}/usage', binary_body=False, binary_response=False, options=options))

    def run_shell(self, id: str, body: T.ShellRequest, *, options: RequestOptions | None = None) -> None:
        """POST /v1/threads/{id}/shell."""
        return cast("None", self.request('POST', f'/v1/threads/{self.segment(id)}/shell', body=body, binary_body=False, binary_response=False, options=options))

    def revert_thread(self, id: str, body: T.RevertRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads/{id}/revert."""
        return cast("T.Thread", self.request('POST', f'/v1/threads/{self.segment(id)}/revert', body=body, binary_body=False, binary_response=False, options=options))

    def unrevert_thread(self, id: str, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads/{id}/unrevert."""
        return cast("T.Thread", self.request('POST', f'/v1/threads/{self.segment(id)}/unrevert', binary_body=False, binary_response=False, options=options))

    def last_turn_diff(self, id: str, *, options: RequestOptions | None = None) -> list[T.FileDiff]:
        """GET /v1/threads/{id}/diff."""
        return cast("list[T.FileDiff]", self.request('GET', f'/v1/threads/{self.segment(id)}/diff', binary_body=False, binary_response=False, options=options))

    def last_turn_review(self, id: str, query: T.DiffReviewQuery | None = None, *, options: RequestOptions | None = None) -> list[T.FileDiff]:
        """GET /v1/threads/{id}/diff/review."""
        return cast("list[T.FileDiff]", self.request('GET', f'/v1/threads/{self.segment(id)}/diff/review', query=query, binary_body=False, binary_response=False, options=options))

    def vcs_diff(self, query: T.VcsDiffQuery | None = None, *, options: RequestOptions | None = None) -> list[T.ReviewFile]:
        """GET /v1/vcs/diff."""
        return cast("list[T.ReviewFile]", self.request('GET', f'/v1/vcs/diff', query=query, binary_body=False, binary_response=False, options=options))

    def search_files(self, query: T.FileSearchQuery, *, options: RequestOptions | None = None) -> list[str]:
        """GET /v1/files/search."""
        return cast("list[str]", self.request('GET', f'/v1/files/search', query=query, binary_body=False, binary_response=False, options=options))

    def import_session_archive(self, body: T.SessionArchiveImportRequest, *, options: RequestOptions | None = None) -> T.SessionArchiveImportResult:
        """POST /v1/session-archives/import."""
        return cast("T.SessionArchiveImportResult", self.request('POST', f'/v1/session-archives/import', body=body, binary_body=False, binary_response=False, options=options))

    def active_turns(self, *, options: RequestOptions | None = None) -> list[T.Turn]:
        """GET /v1/turns/active."""
        return cast("list[T.Turn]", self.request('GET', f'/v1/turns/active', binary_body=False, binary_response=False, options=options))

    def get_turn(self, id: str, *, options: RequestOptions | None = None) -> T.Turn:
        """GET /v1/turns/{id}."""
        return cast("T.Turn", self.request('GET', f'/v1/turns/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def interrupt_turn(self, id: str, query: T.InterruptQuery | None = None, *, options: RequestOptions | None = None) -> T.Turn:
        """POST /v1/turns/{id}/interrupt."""
        return cast("T.Turn", self.request('POST', f'/v1/turns/{self.segment(id)}/interrupt', query=query, binary_body=False, binary_response=False, options=options))

    def wait_turn(self, id: str, query: T.WaitQuery | None = None, *, options: RequestOptions | None = None) -> T.Turn:
        """GET /v1/turns/{id}/wait."""
        return cast("T.Turn", self.request('GET', f'/v1/turns/{self.segment(id)}/wait', query=query, binary_body=False, binary_response=False, options=options))

    def list_permissions(self, *, options: RequestOptions | None = None) -> list[T.PendingPermissionInfo]:
        """GET /v1/permissions."""
        return cast("list[T.PendingPermissionInfo]", self.request('GET', f'/v1/permissions', binary_body=False, binary_response=False, options=options))

    def reply_permission(self, id: str, body: T.PermissionReply, *, options: RequestOptions | None = None) -> None:
        """POST /v1/permissions/{id}."""
        return cast("None", self.request('POST', f'/v1/permissions/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    def list_questions(self, *, options: RequestOptions | None = None) -> list[T.PendingQuestionInfo]:
        """GET /v1/questions."""
        return cast("list[T.PendingQuestionInfo]", self.request('GET', f'/v1/questions', binary_body=False, binary_response=False, options=options))

    def reply_question(self, id: str, body: T.QuestionReply, *, options: RequestOptions | None = None) -> None:
        """POST /v1/questions/{id}."""
        return cast("None", self.request('POST', f'/v1/questions/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    def shutdown_server(self, *, options: RequestOptions | None = None) -> None:
        """POST /v1/shutdown."""
        return cast("None", self.request('POST', f'/v1/shutdown', binary_body=False, binary_response=False, options=options))

    def health(self, *, options: RequestOptions | None = None) -> T.HealthResponse:
        """GET /healthz."""
        return cast("T.HealthResponse", self.request('GET', f'/healthz', binary_body=False, binary_response=False, options=options))

    def openapi(self, *, options: RequestOptions | None = None) -> T.JsonValue:
        """GET /openapi.json."""
        return cast("T.JsonValue", self.request('GET', f'/openapi.json', binary_body=False, binary_response=False, options=options))

    def list_profiles(self, query: T.Filters | None = None, *, options: RequestOptions | None = None) -> list[T.ProfileRecord]:
        """GET /v1/profiles."""
        return cast("list[T.ProfileRecord]", self.request('GET', f'/v1/profiles', query=query, binary_body=False, binary_response=False, options=options))

    def upload_profile(self, body: bytes, *, options: RequestOptions | None = None) -> T.ProfileRecord:
        """POST /v1/profiles."""
        return cast("T.ProfileRecord", self.request('POST', f'/v1/profiles', body=body, binary_body=True, binary_response=False, options=options))

    def get_profile(self, id: str, *, options: RequestOptions | None = None) -> T.ProfileRecord:
        """GET /v1/profiles/{id}."""
        return cast("T.ProfileRecord", self.request('GET', f'/v1/profiles/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def delete_profile(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/profiles/{id}."""
        return cast("None", self.request('DELETE', f'/v1/profiles/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def download_profile(self, id: str, *, options: RequestOptions | None = None) -> bytes:
        """GET /v1/profiles/{id}/download."""
        return cast("bytes", self.request('GET', f'/v1/profiles/{self.segment(id)}/download', binary_body=False, binary_response=True, options=options))

    def list_files(self, query: T.FileQuery | None = None, *, options: RequestOptions | None = None) -> T.FileListing:
        """GET /v1/files/list."""
        return cast("T.FileListing", self.request('GET', f'/v1/files/list', query=query, binary_body=False, binary_response=False, options=options))

    def read_file(self, query: T.FileQuery | None = None, *, options: RequestOptions | None = None) -> T.FileContent:
        """GET /v1/files/content."""
        return cast("T.FileContent", self.request('GET', f'/v1/files/content', query=query, binary_body=False, binary_response=False, options=options))

    def write_file(self, body: T.FileWrite, query: T.FileScope | None = None, *, options: RequestOptions | None = None) -> T.FileContent:
        """PUT /v1/files/content."""
        return cast("T.FileContent", self.request('PUT', f'/v1/files/content', body=body, query=query, binary_body=False, binary_response=False, options=options))

    def mutate_file(self, body: T.FileAction, query: T.FileScope | None = None, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/files/action."""
        return cast("T.JsonValue", self.request('POST', f'/v1/files/action', body=body, query=query, binary_body=False, binary_response=False, options=options))

    def preview_version(self, query: T.PreviewVersionQuery, *, options: RequestOptions | None = None) -> T.FileContent:
        """GET /v1/files/preview."""
        return cast("T.FileContent", self.request('GET', f'/v1/files/preview', query=query, binary_body=False, binary_response=False, options=options))

    def current_preview(self, query: T.FileQuery | None = None, *, options: RequestOptions | None = None) -> T.PreviewCurrent:
        """GET /v1/files/preview/current."""
        return cast("T.PreviewCurrent", self.request('GET', f'/v1/files/preview/current', query=query, binary_body=False, binary_response=False, options=options))

    def restore_preview(self, body: T.PreviewRestoreRequest, *, options: RequestOptions | None = None) -> T.PreviewRestoreResult:
        """POST /v1/files/preview/restore."""
        return cast("T.PreviewRestoreResult", self.request('POST', f'/v1/files/preview/restore', body=body, binary_body=False, binary_response=False, options=options))

    def browser_tunnel(self, id: str, action: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/threads/{id}/browser/tunnel/{action}."""
        return cast("T.JsonValue", self.request('POST', f'/v1/threads/{self.segment(id)}/browser/tunnel/{self.segment(action)}', body=body, binary_body=False, binary_response=False, options=options))

    def call_browser_tool(self, id: str, body: T.BrowserToolRequest, *, options: RequestOptions | None = None) -> T.BrowserToolResponse:
        """POST /v1/threads/{id}/browser/tool."""
        return cast("T.BrowserToolResponse", self.request('POST', f'/v1/threads/{self.segment(id)}/browser/tool', body=body, binary_body=False, binary_response=False, options=options))

    def draft_browser_tunnel(self, id: str, action: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/browser/drafts/{id}/tunnel/{action}."""
        return cast("T.JsonValue", self.request('POST', f'/v1/browser/drafts/{self.segment(id)}/tunnel/{self.segment(action)}', body=body, binary_body=False, binary_response=False, options=options))

    def adopt_browser_draft(self, id: str, body: T.AdoptDraft, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/browser/drafts/{id}/adopt."""
        return cast("T.JsonValue", self.request('POST', f'/v1/browser/drafts/{self.segment(id)}/adopt', body=body, binary_body=False, binary_response=False, options=options))

    def list_browser_hosts(self, query: T.BrowserHostsQuery | None = None, *, options: RequestOptions | None = None) -> T.BrowserHostsResponse:
        """GET /v1/browser/hosts."""
        return cast("T.BrowserHostsResponse", self.request('GET', f'/v1/browser/hosts', query=query, binary_body=False, binary_response=False, options=options))

    def host_projects(self, *, options: RequestOptions | None = None) -> T.WorkspaceListResponse:
        """GET /v1/host/projects."""
        return cast("T.WorkspaceListResponse", self.request('GET', f'/v1/host/projects', binary_body=False, binary_response=False, options=options))

    def host_directories(self, query: T.Browse | None = None, *, options: RequestOptions | None = None) -> T.DirectoryListing:
        """GET /v1/host/directories."""
        return cast("T.DirectoryListing", self.request('GET', f'/v1/host/directories', query=query, binary_body=False, binary_response=False, options=options))

    def host_open(self, body: T.Open, *, options: RequestOptions | None = None) -> T.WorkspaceInfo:
        """POST /v1/host/open."""
        return cast("T.WorkspaceInfo", self.request('POST', f'/v1/host/open', body=body, binary_body=False, binary_response=False, options=options))

    def host_close(self, workspace_id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/host/projects/{workspace_id}."""
        return cast("None", self.request('DELETE', f'/v1/host/projects/{self.segment(workspace_id)}', binary_body=False, binary_response=False, options=options))

    def read_shared_session(self, token: str, *, options: RequestOptions | None = None) -> str:
        """GET /s/{token}."""
        return cast("str", self.request('GET', f'/s/{self.segment(token)}', text_response=True, binary_body=False, binary_response=False, options=options))

class AsyncApi(AsyncTransport):
    async def manage_resource(self, resource: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/manage/{resource}."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/manage/{self.segment(resource)}', body=body, binary_body=False, binary_response=False, options=options))

    async def cloud_agents(self, body: T.CloudAgentInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/agents."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/cloud/agents', body=body, binary_body=False, binary_response=False, options=options))

    async def cloud_workspaces(self, body: T.CloudWorkspaceInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/workspaces."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/cloud/workspaces', body=body, binary_body=False, binary_response=False, options=options))

    async def cloud_chats(self, body: T.CloudChatInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/chats."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/cloud/chats', body=body, binary_body=False, binary_response=False, options=options))

    async def cloud_schedules(self, body: T.CloudScheduleInput, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/cloud/schedules."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/cloud/schedules', body=body, binary_body=False, binary_response=False, options=options))

    async def info(self, *, options: RequestOptions | None = None) -> T.ServerInfo:
        """GET /v1/info."""
        return cast("T.ServerInfo", await self.request('GET', f'/v1/info', binary_body=False, binary_response=False, options=options))

    async def analytics_stats(self, query: T.AnalyticsQuery | None = None, *, options: RequestOptions | None = None) -> T.Report:
        """GET /v1/stats."""
        return cast("T.Report", await self.request('GET', f'/v1/stats', query=query, binary_body=False, binary_response=False, options=options))

    async def analytics_insights(self, query: T.AnalyticsQuery | None = None, *, options: RequestOptions | None = None) -> T.AnalyticsDocument:
        """GET /v1/insights."""
        return cast("T.AnalyticsDocument", await self.request('GET', f'/v1/insights', query=query, binary_body=False, binary_response=False, options=options))

    async def providers(self, *, options: RequestOptions | None = None) -> list[T.ProviderInfo]:
        """GET /v1/providers."""
        return cast("list[T.ProviderInfo]", await self.request('GET', f'/v1/providers', binary_body=False, binary_response=False, options=options))

    async def refresh_providers(self, *, options: RequestOptions | None = None) -> list[T.ProviderInfo]:
        """POST /v1/providers/refresh."""
        return cast("list[T.ProviderInfo]", await self.request('POST', f'/v1/providers/refresh', binary_body=False, binary_response=False, options=options))

    async def agents(self, *, options: RequestOptions | None = None) -> list[T.AgentProfile]:
        """GET /v1/agents."""
        return cast("list[T.AgentProfile]", await self.request('GET', f'/v1/agents', binary_body=False, binary_response=False, options=options))

    async def commands(self, query: T.ChatListQuery | None = None, *, options: RequestOptions | None = None) -> list[T.CommandInfo]:
        """GET /v1/commands."""
        return cast("list[T.CommandInfo]", await self.request('GET', f'/v1/commands', query=query, binary_body=False, binary_response=False, options=options))

    async def lsp_status(self, *, options: RequestOptions | None = None) -> list[T.LanguageStatus]:
        """GET /v1/lsp."""
        return cast("list[T.LanguageStatus]", await self.request('GET', f'/v1/lsp', binary_body=False, binary_response=False, options=options))

    async def formatter_status(self, *, options: RequestOptions | None = None) -> list[T.FormatterStatus]:
        """GET /v1/formatter."""
        return cast("list[T.FormatterStatus]", await self.request('GET', f'/v1/formatter', binary_body=False, binary_response=False, options=options))

    async def tool_catalog(self, query: T.ToolCatalogQuery | None = None, *, options: RequestOptions | None = None) -> T.ToolCatalogResponse:
        """GET /v1/tools."""
        return cast("T.ToolCatalogResponse", await self.request('GET', f'/v1/tools', query=query, binary_body=False, binary_response=False, options=options))

    async def update_lazy_tools(self, body: T.LazyToolSettingsUpdate, *, options: RequestOptions | None = None) -> T.ToolCatalogResponse:
        """POST /v1/tools/lazy."""
        return cast("T.ToolCatalogResponse", await self.request('POST', f'/v1/tools/lazy', body=body, binary_body=False, binary_response=False, options=options))

    async def update_tool(self, name: str, body: T.ToolSettingsUpdate, *, options: RequestOptions | None = None) -> T.ToolCatalogResponse:
        """POST /v1/tools/{name}."""
        return cast("T.ToolCatalogResponse", await self.request('POST', f'/v1/tools/{self.segment(name)}', body=body, binary_body=False, binary_response=False, options=options))

    async def projects(self, *, options: RequestOptions | None = None) -> list[T.ProjectInfo]:
        """GET /v1/projects."""
        return cast("list[T.ProjectInfo]", await self.request('GET', f'/v1/projects', binary_body=False, binary_response=False, options=options))

    async def workspaces_list(self, *, options: RequestOptions | None = None) -> T.WorkspaceListResponse:
        """GET /v1/workspaces."""
        return cast("T.WorkspaceListResponse", await self.request('GET', f'/v1/workspaces', binary_body=False, binary_response=False, options=options))

    async def create_workspace(self, body: T.WorkspaceCreateRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/v1/workspaces', body=body, binary_body=False, binary_response=False, options=options))

    async def workspace_refs(self, *, options: RequestOptions | None = None) -> T.WorkspaceRefsResponse:
        """GET /v1/workspaces/refs."""
        return cast("T.WorkspaceRefsResponse", await self.request('GET', f'/v1/workspaces/refs', binary_body=False, binary_response=False, options=options))

    async def resolve_workspace_ref(self, body: T.WorkspaceResolveRefRequest, *, options: RequestOptions | None = None) -> T.WorkspaceResolveRefResponse:
        """POST /v1/workspaces/resolve-ref."""
        return cast("T.WorkspaceResolveRefResponse", await self.request('POST', f'/v1/workspaces/resolve-ref', body=body, binary_body=False, binary_response=False, options=options))

    async def get_workspace(self, reference: str, *, options: RequestOptions | None = None) -> T.WorkspaceInfo:
        """GET /v1/workspaces/{reference}."""
        return cast("T.WorkspaceInfo", await self.request('GET', f'/v1/workspaces/{self.segment(reference)}', binary_body=False, binary_response=False, options=options))

    async def remove_workspace(self, reference: str, body: T.WorkspaceRemoveRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """DELETE /v1/workspaces/{reference}."""
        return cast("T.WorkspaceOperationResponse", await self.request('DELETE', f'/v1/workspaces/{self.segment(reference)}', body=body, binary_body=False, binary_response=False, options=options))

    async def open_workspace(self, reference: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces/{reference}/open."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/v1/workspaces/{self.segment(reference)}/open', binary_body=False, binary_response=False, options=options))

    async def reset_workspace(self, reference: str, body: T.WorkspaceResetRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces/{reference}/reset."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/v1/workspaces/{self.segment(reference)}/reset', body=body, binary_body=False, binary_response=False, options=options))

    async def cleanup_workspace(self, reference: str, body: T.WorkspaceCleanupRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /v1/workspaces/{reference}/cleanup."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/v1/workspaces/{self.segment(reference)}/cleanup', body=body, binary_body=False, binary_response=False, options=options))

    async def list_ptys(self, *, options: RequestOptions | None = None) -> list[T.PtyInfo]:
        """GET /v1/ptys."""
        return cast("list[T.PtyInfo]", await self.request('GET', f'/v1/ptys', binary_body=False, binary_response=False, options=options))

    async def create_pty(self, body: T.PtyCreateRequest, *, options: RequestOptions | None = None) -> T.PtyInfo:
        """POST /v1/ptys."""
        return cast("T.PtyInfo", await self.request('POST', f'/v1/ptys', body=body, binary_body=False, binary_response=False, options=options))

    async def get_pty(self, id: str, *, options: RequestOptions | None = None) -> T.PtyInfo:
        """GET /v1/ptys/{id}."""
        return cast("T.PtyInfo", await self.request('GET', f'/v1/ptys/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_pty(self, id: str, body: T.PtyUpdateRequest, *, options: RequestOptions | None = None) -> T.PtyInfo:
        """PATCH /v1/ptys/{id}."""
        return cast("T.PtyInfo", await self.request('PATCH', f'/v1/ptys/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    async def delete_pty(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/ptys/{id}."""
        return cast("None", await self.request('DELETE', f'/v1/ptys/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def workflows(self, query: T.ChatListQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkflowListResponse:
        """GET /v1/workflows."""
        return cast("T.WorkflowListResponse", await self.request('GET', f'/v1/workflows', query=query, binary_body=False, binary_response=False, options=options))

    async def workflow_source(self, invocation: str, *, options: RequestOptions | None = None) -> T.WorkflowSourceResponse:
        """GET /v1/workflows/{invocation}/source."""
        return cast("T.WorkflowSourceResponse", await self.request('GET', f'/v1/workflows/{self.segment(invocation)}/source', binary_body=False, binary_response=False, options=options))

    async def start_workflow(self, body: T.WorkflowStartRequest, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/run."""
        return cast("T.WorkflowRun", await self.request('POST', f'/v1/workflows/run', body=body, binary_body=False, binary_response=False, options=options))

    async def list_workflow_runs(self, query: T.WorkflowRunsQuery | None = None, *, options: RequestOptions | None = None) -> list[T.WorkflowRun]:
        """GET /v1/workflows/runs."""
        return cast("list[T.WorkflowRun]", await self.request('GET', f'/v1/workflows/runs', query=query, binary_body=False, binary_response=False, options=options))

    async def get_workflow_run(self, id: str, query: T.WorkflowRunQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """GET /v1/workflows/runs/{id}."""
        return cast("T.WorkflowRun", await self.request('GET', f'/v1/workflows/runs/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    async def delete_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/workflows/runs/{id}."""
        return cast("None", await self.request('DELETE', f'/v1/workflows/runs/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def wait_workflow_run(self, id: str, query: T.WorkflowWaitQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """GET /v1/workflows/runs/{id}/wait."""
        return cast("T.WorkflowRun", await self.request('GET', f'/v1/workflows/runs/{self.segment(id)}/wait', query=query, binary_body=False, binary_response=False, options=options))

    async def cancel_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/runs/{id}/cancel."""
        return cast("T.WorkflowRun", await self.request('POST', f'/v1/workflows/runs/{self.segment(id)}/cancel', binary_body=False, binary_response=False, options=options))

    async def pause_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/runs/{id}/pause."""
        return cast("T.WorkflowRun", await self.request('POST', f'/v1/workflows/runs/{self.segment(id)}/pause', binary_body=False, binary_response=False, options=options))

    async def resume_workflow_run(self, id: str, *, options: RequestOptions | None = None) -> T.WorkflowRun:
        """POST /v1/workflows/runs/{id}/resume."""
        return cast("T.WorkflowRun", await self.request('POST', f'/v1/workflows/runs/{self.segment(id)}/resume', binary_body=False, binary_response=False, options=options))

    async def skills(self, *, options: RequestOptions | None = None) -> list[T.SkillInfo]:
        """GET /v1/skills."""
        return cast("list[T.SkillInfo]", await self.request('GET', f'/v1/skills', binary_body=False, binary_response=False, options=options))

    async def load_skill(self, name: str, *, options: RequestOptions | None = None) -> T.SkillContent:
        """GET /v1/skills/{name}."""
        return cast("T.SkillContent", await self.request('GET', f'/v1/skills/{self.segment(name)}', binary_body=False, binary_response=False, options=options))

    async def plugins(self, *, options: RequestOptions | None = None) -> list[T.PluginInfo]:
        """GET /v1/plugins."""
        return cast("list[T.PluginInfo]", await self.request('GET', f'/v1/plugins', binary_body=False, binary_response=False, options=options))

    async def plugin_action(self, body: T.PluginActionRequest, *, options: RequestOptions | None = None) -> list[T.PluginInfo]:
        """POST /v1/plugins."""
        return cast("list[T.PluginInfo]", await self.request('POST', f'/v1/plugins', body=body, binary_body=False, binary_response=False, options=options))

    async def mcp_servers(self, query: T.McpServersQuery | None = None, *, options: RequestOptions | None = None) -> list[T.McpServerInfo]:
        """GET /v1/mcp."""
        return cast("list[T.McpServerInfo]", await self.request('GET', f'/v1/mcp', query=query, binary_body=False, binary_response=False, options=options))

    async def configure_mcp(self, name: str, body: T.McpConfigureRequest, *, options: RequestOptions | None = None) -> T.McpServerInfo:
        """POST /v1/mcp/{name}."""
        return cast("T.McpServerInfo", await self.request('POST', f'/v1/mcp/{self.segment(name)}', body=body, binary_body=False, binary_response=False, options=options))

    async def reconnect_mcp(self, name: str, *, options: RequestOptions | None = None) -> T.McpServerInfo:
        """POST /v1/mcp/{name}/reconnect."""
        return cast("T.McpServerInfo", await self.request('POST', f'/v1/mcp/{self.segment(name)}/reconnect', binary_body=False, binary_response=False, options=options))

    async def disconnect_mcp(self, name: str, *, options: RequestOptions | None = None) -> T.McpServerInfo:
        """POST /v1/mcp/{name}/disconnect."""
        return cast("T.McpServerInfo", await self.request('POST', f'/v1/mcp/{self.segment(name)}/disconnect', binary_body=False, binary_response=False, options=options))

    async def mcp_tools(self, name: str, *, options: RequestOptions | None = None) -> list[T.ToolDefinition]:
        """GET /v1/mcp/{name}/tools."""
        return cast("list[T.ToolDefinition]", await self.request('GET', f'/v1/mcp/{self.segment(name)}/tools', binary_body=False, binary_response=False, options=options))

    async def call_mcp_tool(self, name: str, tool: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.ToolOutput:
        """POST /v1/mcp/{name}/tools/{tool}."""
        return cast("T.ToolOutput", await self.request('POST', f'/v1/mcp/{self.segment(name)}/tools/{self.segment(tool)}', body=body, binary_body=False, binary_response=False, options=options))

    async def mcp_resources(self, name: str, *, options: RequestOptions | None = None) -> list[T.JsonValue]:
        """GET /v1/mcp/{name}/resources."""
        return cast("list[T.JsonValue]", await self.request('GET', f'/v1/mcp/{self.segment(name)}/resources', binary_body=False, binary_response=False, options=options))

    async def mcp_resource_templates(self, name: str, *, options: RequestOptions | None = None) -> list[T.JsonValue]:
        """GET /v1/mcp/{name}/resource-templates."""
        return cast("list[T.JsonValue]", await self.request('GET', f'/v1/mcp/{self.segment(name)}/resource-templates', binary_body=False, binary_response=False, options=options))

    async def read_mcp_resource(self, name: str, body: T.ResourceReadRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/mcp/{name}/resources/read."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/mcp/{self.segment(name)}/resources/read', body=body, binary_body=False, binary_response=False, options=options))

    async def mcp_prompts(self, name: str, *, options: RequestOptions | None = None) -> list[T.JsonValue]:
        """GET /v1/mcp/{name}/prompts."""
        return cast("list[T.JsonValue]", await self.request('GET', f'/v1/mcp/{self.segment(name)}/prompts', binary_body=False, binary_response=False, options=options))

    async def get_mcp_prompt(self, name: str, prompt: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/mcp/{name}/prompts/{prompt}."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/mcp/{self.segment(name)}/prompts/{self.segment(prompt)}', body=body, binary_body=False, binary_response=False, options=options))

    async def list_threads(self, query: T.ThreadQuery | None = None, *, options: RequestOptions | None = None) -> list[T.Thread]:
        """GET /v1/threads."""
        return cast("list[T.Thread]", await self.request('GET', f'/v1/threads', query=query, binary_body=False, binary_response=False, options=options))

    async def create_thread(self, body: T.CreateThreadRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads."""
        return cast("T.Thread", await self.request('POST', f'/v1/threads', body=body, binary_body=False, binary_response=False, options=options))

    async def get_thread(self, id: str, *, options: RequestOptions | None = None) -> T.Thread:
        """GET /v1/threads/{id}."""
        return cast("T.Thread", await self.request('GET', f'/v1/threads/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_thread(self, id: str, body: T.UpdateThreadRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """PATCH /v1/threads/{id}."""
        return cast("T.Thread", await self.request('PATCH', f'/v1/threads/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    async def export_session_archive(self, id: str, query: T.SessionArchiveQuery | None = None, *, options: RequestOptions | None = None) -> T.SessionArchive:
        """GET /v1/threads/{id}/archive."""
        return cast("T.SessionArchive", await self.request('GET', f'/v1/threads/{self.segment(id)}/archive', query=query, binary_body=False, binary_response=False, options=options))

    async def fork_thread(self, id: str, body: T.ForkThreadRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads/{id}/fork."""
        return cast("T.Thread", await self.request('POST', f'/v1/threads/{self.segment(id)}/fork', body=body, binary_body=False, binary_response=False, options=options))

    async def list_items(self, id: str, query: T.ItemsQuery | None = None, *, options: RequestOptions | None = None) -> list[T.Item]:
        """GET /v1/threads/{id}/items."""
        return cast("list[T.Item]", await self.request('GET', f'/v1/threads/{self.segment(id)}/items', query=query, binary_body=False, binary_response=False, options=options))

    async def list_todos(self, id: str, *, options: RequestOptions | None = None) -> list[T.TodoEntry]:
        """GET /v1/threads/{id}/todos."""
        return cast("list[T.TodoEntry]", await self.request('GET', f'/v1/threads/{self.segment(id)}/todos', binary_body=False, binary_response=False, options=options))

    async def active_turn(self, id: str, *, options: RequestOptions | None = None) -> Union[T.Turn, None]:
        """GET /v1/threads/{id}/active."""
        return cast("Union[T.Turn, None]", await self.request('GET', f'/v1/threads/{self.segment(id)}/active', binary_body=False, binary_response=False, options=options))

    async def thread_activity(self, id: str, *, options: RequestOptions | None = None) -> T.ThreadActivity:
        """GET /v1/threads/{id}/activity."""
        return cast("T.ThreadActivity", await self.request('GET', f'/v1/threads/{self.segment(id)}/activity', binary_body=False, binary_response=False, options=options))

    async def interrupt_thread_tree(self, id: str, query: T.InterruptQuery | None = None, *, options: RequestOptions | None = None) -> T.InterruptResponse:
        """POST /v1/threads/{id}/interrupt."""
        return cast("T.InterruptResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/interrupt', query=query, binary_body=False, binary_response=False, options=options))

    async def list_background_tasks(self, id: str, *, options: RequestOptions | None = None) -> list[T.BackgroundTaskInfo]:
        """GET /v1/threads/{id}/background."""
        return cast("list[T.BackgroundTaskInfo]", await self.request('GET', f'/v1/threads/{self.segment(id)}/background', binary_body=False, binary_response=False, options=options))

    async def promote_background_tasks(self, id: str, query: T.BackgroundQuery | None = None, *, options: RequestOptions | None = None) -> list[T.BackgroundTaskInfo]:
        """POST /v1/threads/{id}/background."""
        return cast("list[T.BackgroundTaskInfo]", await self.request('POST', f'/v1/threads/{self.segment(id)}/background', query=query, binary_body=False, binary_response=False, options=options))

    async def pending_question(self, id: str, *, options: RequestOptions | None = None) -> Union[T.PendingQuestionInfo, None]:
        """GET /v1/threads/{id}/pending-question."""
        return cast("Union[T.PendingQuestionInfo, None]", await self.request('GET', f'/v1/threads/{self.segment(id)}/pending-question', binary_body=False, binary_response=False, options=options))

    async def start_turn(self, id: str, body: T.StartTurnRequest, *, options: RequestOptions | None = None) -> T.Turn:
        """POST /v1/threads/{id}/turns."""
        return cast("T.Turn", await self.request('POST', f'/v1/threads/{self.segment(id)}/turns', body=body, binary_body=False, binary_response=False, options=options))

    async def execute_command(self, id: str, body: T.ExecuteCommandRequest, *, options: RequestOptions | None = None) -> T.CommandExecutionResponse:
        """POST /v1/threads/{id}/commands."""
        return cast("T.CommandExecutionResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/commands', body=body, binary_body=False, binary_response=False, options=options))

    async def queue_input(self, id: str, body: T.QueueInputRequest, *, options: RequestOptions | None = None) -> T.QueueInputResponse:
        """POST /v1/threads/{id}/input."""
        return cast("T.QueueInputResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/input', body=body, binary_body=False, binary_response=False, options=options))

    async def input_queue(self, id: str, *, options: RequestOptions | None = None) -> T.InputQueue:
        """GET /v1/threads/{id}/input/queue."""
        return cast("T.InputQueue", await self.request('GET', f'/v1/threads/{self.segment(id)}/input/queue', binary_body=False, binary_response=False, options=options))

    async def change_input_queue(self, id: str, body: T.InputQueueAction, *, options: RequestOptions | None = None) -> T.InputQueue:
        """POST /v1/threads/{id}/input/queue."""
        return cast("T.InputQueue", await self.request('POST', f'/v1/threads/{self.segment(id)}/input/queue', body=body, binary_body=False, binary_response=False, options=options))

    async def btw(self, id: str, body: T.BtwRequest, *, options: RequestOptions | None = None) -> T.BtwResponse:
        """POST /v1/threads/{id}/btw."""
        return cast("T.BtwResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/btw', body=body, binary_body=False, binary_response=False, options=options))

    async def compact(self, id: str, body: T.CompactRequest, *, options: RequestOptions | None = None) -> T.Item:
        """POST /v1/threads/{id}/compact."""
        return cast("T.Item", await self.request('POST', f'/v1/threads/{self.segment(id)}/compact', body=body, binary_body=False, binary_response=False, options=options))

    async def get_goal(self, id: str, *, options: RequestOptions | None = None) -> T.GoalActionResponse:
        """GET /v1/threads/{id}/goal."""
        return cast("T.GoalActionResponse", await self.request('GET', f'/v1/threads/{self.segment(id)}/goal', binary_body=False, binary_response=False, options=options))

    async def set_goal(self, id: str, body: T.GoalRequest, *, options: RequestOptions | None = None) -> T.GoalActionResponse:
        """POST /v1/threads/{id}/goal."""
        return cast("T.GoalActionResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/goal', body=body, binary_body=False, binary_response=False, options=options))

    async def get_approval_mode(self, id: str, *, options: RequestOptions | None = None) -> T.ApprovalModeInfo:
        """GET /v1/threads/{id}/approval-mode."""
        return cast("T.ApprovalModeInfo", await self.request('GET', f'/v1/threads/{self.segment(id)}/approval-mode', binary_body=False, binary_response=False, options=options))

    async def set_approval_mode(self, id: str, body: T.SetApprovalModeRequest, *, options: RequestOptions | None = None) -> T.ApprovalModeInfo:
        """PUT /v1/threads/{id}/approval-mode."""
        return cast("T.ApprovalModeInfo", await self.request('PUT', f'/v1/threads/{self.segment(id)}/approval-mode', body=body, binary_body=False, binary_response=False, options=options))

    async def get_loop(self, id: str, *, options: RequestOptions | None = None) -> T.LoopResponse:
        """GET /v1/threads/{id}/loop."""
        return cast("T.LoopResponse", await self.request('GET', f'/v1/threads/{self.segment(id)}/loop', binary_body=False, binary_response=False, options=options))

    async def set_loop(self, id: str, body: T.LoopRequest, *, options: RequestOptions | None = None) -> T.LoopResponse:
        """POST /v1/threads/{id}/loop."""
        return cast("T.LoopResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/loop', body=body, binary_body=False, binary_response=False, options=options))

    async def thread_usage(self, id: str, *, options: RequestOptions | None = None) -> T.UsageResponse:
        """GET /v1/threads/{id}/usage."""
        return cast("T.UsageResponse", await self.request('GET', f'/v1/threads/{self.segment(id)}/usage', binary_body=False, binary_response=False, options=options))

    async def run_shell(self, id: str, body: T.ShellRequest, *, options: RequestOptions | None = None) -> None:
        """POST /v1/threads/{id}/shell."""
        return cast("None", await self.request('POST', f'/v1/threads/{self.segment(id)}/shell', body=body, binary_body=False, binary_response=False, options=options))

    async def revert_thread(self, id: str, body: T.RevertRequest, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads/{id}/revert."""
        return cast("T.Thread", await self.request('POST', f'/v1/threads/{self.segment(id)}/revert', body=body, binary_body=False, binary_response=False, options=options))

    async def unrevert_thread(self, id: str, *, options: RequestOptions | None = None) -> T.Thread:
        """POST /v1/threads/{id}/unrevert."""
        return cast("T.Thread", await self.request('POST', f'/v1/threads/{self.segment(id)}/unrevert', binary_body=False, binary_response=False, options=options))

    async def last_turn_diff(self, id: str, *, options: RequestOptions | None = None) -> list[T.FileDiff]:
        """GET /v1/threads/{id}/diff."""
        return cast("list[T.FileDiff]", await self.request('GET', f'/v1/threads/{self.segment(id)}/diff', binary_body=False, binary_response=False, options=options))

    async def last_turn_review(self, id: str, query: T.DiffReviewQuery | None = None, *, options: RequestOptions | None = None) -> list[T.FileDiff]:
        """GET /v1/threads/{id}/diff/review."""
        return cast("list[T.FileDiff]", await self.request('GET', f'/v1/threads/{self.segment(id)}/diff/review', query=query, binary_body=False, binary_response=False, options=options))

    async def vcs_diff(self, query: T.VcsDiffQuery | None = None, *, options: RequestOptions | None = None) -> list[T.ReviewFile]:
        """GET /v1/vcs/diff."""
        return cast("list[T.ReviewFile]", await self.request('GET', f'/v1/vcs/diff', query=query, binary_body=False, binary_response=False, options=options))

    async def search_files(self, query: T.FileSearchQuery, *, options: RequestOptions | None = None) -> list[str]:
        """GET /v1/files/search."""
        return cast("list[str]", await self.request('GET', f'/v1/files/search', query=query, binary_body=False, binary_response=False, options=options))

    async def import_session_archive(self, body: T.SessionArchiveImportRequest, *, options: RequestOptions | None = None) -> T.SessionArchiveImportResult:
        """POST /v1/session-archives/import."""
        return cast("T.SessionArchiveImportResult", await self.request('POST', f'/v1/session-archives/import', body=body, binary_body=False, binary_response=False, options=options))

    async def active_turns(self, *, options: RequestOptions | None = None) -> list[T.Turn]:
        """GET /v1/turns/active."""
        return cast("list[T.Turn]", await self.request('GET', f'/v1/turns/active', binary_body=False, binary_response=False, options=options))

    async def get_turn(self, id: str, *, options: RequestOptions | None = None) -> T.Turn:
        """GET /v1/turns/{id}."""
        return cast("T.Turn", await self.request('GET', f'/v1/turns/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def interrupt_turn(self, id: str, query: T.InterruptQuery | None = None, *, options: RequestOptions | None = None) -> T.Turn:
        """POST /v1/turns/{id}/interrupt."""
        return cast("T.Turn", await self.request('POST', f'/v1/turns/{self.segment(id)}/interrupt', query=query, binary_body=False, binary_response=False, options=options))

    async def wait_turn(self, id: str, query: T.WaitQuery | None = None, *, options: RequestOptions | None = None) -> T.Turn:
        """GET /v1/turns/{id}/wait."""
        return cast("T.Turn", await self.request('GET', f'/v1/turns/{self.segment(id)}/wait', query=query, binary_body=False, binary_response=False, options=options))

    async def list_permissions(self, *, options: RequestOptions | None = None) -> list[T.PendingPermissionInfo]:
        """GET /v1/permissions."""
        return cast("list[T.PendingPermissionInfo]", await self.request('GET', f'/v1/permissions', binary_body=False, binary_response=False, options=options))

    async def reply_permission(self, id: str, body: T.PermissionReply, *, options: RequestOptions | None = None) -> None:
        """POST /v1/permissions/{id}."""
        return cast("None", await self.request('POST', f'/v1/permissions/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    async def list_questions(self, *, options: RequestOptions | None = None) -> list[T.PendingQuestionInfo]:
        """GET /v1/questions."""
        return cast("list[T.PendingQuestionInfo]", await self.request('GET', f'/v1/questions', binary_body=False, binary_response=False, options=options))

    async def reply_question(self, id: str, body: T.QuestionReply, *, options: RequestOptions | None = None) -> None:
        """POST /v1/questions/{id}."""
        return cast("None", await self.request('POST', f'/v1/questions/{self.segment(id)}', body=body, binary_body=False, binary_response=False, options=options))

    async def shutdown_server(self, *, options: RequestOptions | None = None) -> None:
        """POST /v1/shutdown."""
        return cast("None", await self.request('POST', f'/v1/shutdown', binary_body=False, binary_response=False, options=options))

    async def health(self, *, options: RequestOptions | None = None) -> T.HealthResponse:
        """GET /healthz."""
        return cast("T.HealthResponse", await self.request('GET', f'/healthz', binary_body=False, binary_response=False, options=options))

    async def openapi(self, *, options: RequestOptions | None = None) -> T.JsonValue:
        """GET /openapi.json."""
        return cast("T.JsonValue", await self.request('GET', f'/openapi.json', binary_body=False, binary_response=False, options=options))

    async def list_profiles(self, query: T.Filters | None = None, *, options: RequestOptions | None = None) -> list[T.ProfileRecord]:
        """GET /v1/profiles."""
        return cast("list[T.ProfileRecord]", await self.request('GET', f'/v1/profiles', query=query, binary_body=False, binary_response=False, options=options))

    async def upload_profile(self, body: bytes, *, options: RequestOptions | None = None) -> T.ProfileRecord:
        """POST /v1/profiles."""
        return cast("T.ProfileRecord", await self.request('POST', f'/v1/profiles', body=body, binary_body=True, binary_response=False, options=options))

    async def get_profile(self, id: str, *, options: RequestOptions | None = None) -> T.ProfileRecord:
        """GET /v1/profiles/{id}."""
        return cast("T.ProfileRecord", await self.request('GET', f'/v1/profiles/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def delete_profile(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/profiles/{id}."""
        return cast("None", await self.request('DELETE', f'/v1/profiles/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def download_profile(self, id: str, *, options: RequestOptions | None = None) -> bytes:
        """GET /v1/profiles/{id}/download."""
        return cast("bytes", await self.request('GET', f'/v1/profiles/{self.segment(id)}/download', binary_body=False, binary_response=True, options=options))

    async def list_files(self, query: T.FileQuery | None = None, *, options: RequestOptions | None = None) -> T.FileListing:
        """GET /v1/files/list."""
        return cast("T.FileListing", await self.request('GET', f'/v1/files/list', query=query, binary_body=False, binary_response=False, options=options))

    async def read_file(self, query: T.FileQuery | None = None, *, options: RequestOptions | None = None) -> T.FileContent:
        """GET /v1/files/content."""
        return cast("T.FileContent", await self.request('GET', f'/v1/files/content', query=query, binary_body=False, binary_response=False, options=options))

    async def write_file(self, body: T.FileWrite, query: T.FileScope | None = None, *, options: RequestOptions | None = None) -> T.FileContent:
        """PUT /v1/files/content."""
        return cast("T.FileContent", await self.request('PUT', f'/v1/files/content', body=body, query=query, binary_body=False, binary_response=False, options=options))

    async def mutate_file(self, body: T.FileAction, query: T.FileScope | None = None, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/files/action."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/files/action', body=body, query=query, binary_body=False, binary_response=False, options=options))

    async def preview_version(self, query: T.PreviewVersionQuery, *, options: RequestOptions | None = None) -> T.FileContent:
        """GET /v1/files/preview."""
        return cast("T.FileContent", await self.request('GET', f'/v1/files/preview', query=query, binary_body=False, binary_response=False, options=options))

    async def current_preview(self, query: T.FileQuery | None = None, *, options: RequestOptions | None = None) -> T.PreviewCurrent:
        """GET /v1/files/preview/current."""
        return cast("T.PreviewCurrent", await self.request('GET', f'/v1/files/preview/current', query=query, binary_body=False, binary_response=False, options=options))

    async def restore_preview(self, body: T.PreviewRestoreRequest, *, options: RequestOptions | None = None) -> T.PreviewRestoreResult:
        """POST /v1/files/preview/restore."""
        return cast("T.PreviewRestoreResult", await self.request('POST', f'/v1/files/preview/restore', body=body, binary_body=False, binary_response=False, options=options))

    async def browser_tunnel(self, id: str, action: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/threads/{id}/browser/tunnel/{action}."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/threads/{self.segment(id)}/browser/tunnel/{self.segment(action)}', body=body, binary_body=False, binary_response=False, options=options))

    async def call_browser_tool(self, id: str, body: T.BrowserToolRequest, *, options: RequestOptions | None = None) -> T.BrowserToolResponse:
        """POST /v1/threads/{id}/browser/tool."""
        return cast("T.BrowserToolResponse", await self.request('POST', f'/v1/threads/{self.segment(id)}/browser/tool', body=body, binary_body=False, binary_response=False, options=options))

    async def draft_browser_tunnel(self, id: str, action: str, body: T.JsonValue, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/browser/drafts/{id}/tunnel/{action}."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/browser/drafts/{self.segment(id)}/tunnel/{self.segment(action)}', body=body, binary_body=False, binary_response=False, options=options))

    async def adopt_browser_draft(self, id: str, body: T.AdoptDraft, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /v1/browser/drafts/{id}/adopt."""
        return cast("T.JsonValue", await self.request('POST', f'/v1/browser/drafts/{self.segment(id)}/adopt', body=body, binary_body=False, binary_response=False, options=options))

    async def list_browser_hosts(self, query: T.BrowserHostsQuery | None = None, *, options: RequestOptions | None = None) -> T.BrowserHostsResponse:
        """GET /v1/browser/hosts."""
        return cast("T.BrowserHostsResponse", await self.request('GET', f'/v1/browser/hosts', query=query, binary_body=False, binary_response=False, options=options))

    async def host_projects(self, *, options: RequestOptions | None = None) -> T.WorkspaceListResponse:
        """GET /v1/host/projects."""
        return cast("T.WorkspaceListResponse", await self.request('GET', f'/v1/host/projects', binary_body=False, binary_response=False, options=options))

    async def host_directories(self, query: T.Browse | None = None, *, options: RequestOptions | None = None) -> T.DirectoryListing:
        """GET /v1/host/directories."""
        return cast("T.DirectoryListing", await self.request('GET', f'/v1/host/directories', query=query, binary_body=False, binary_response=False, options=options))

    async def host_open(self, body: T.Open, *, options: RequestOptions | None = None) -> T.WorkspaceInfo:
        """POST /v1/host/open."""
        return cast("T.WorkspaceInfo", await self.request('POST', f'/v1/host/open', body=body, binary_body=False, binary_response=False, options=options))

    async def host_close(self, workspace_id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /v1/host/projects/{workspace_id}."""
        return cast("None", await self.request('DELETE', f'/v1/host/projects/{self.segment(workspace_id)}', binary_body=False, binary_response=False, options=options))

    async def read_shared_session(self, token: str, *, options: RequestOptions | None = None) -> str:
        """GET /s/{token}."""
        return cast("str", await self.request('GET', f'/s/{self.segment(token)}', text_response=True, binary_body=False, binary_response=False, options=options))
